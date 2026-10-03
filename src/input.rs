//! Read original listings and exported snapshots into the same section model.
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::export::EXPORT_COLUMNS;
use crate::{
    Diagnostics, Entry, Listing, Options, Result, Section, is_merged, permissions_record, relative,
    signature,
};

pub enum Input {
    Listing(Box<Listing<BufReader<File>>>),
    Artifact(Box<Artifact>),
}

impl Input {
    pub fn open(path: &Path, options: &Options) -> Result<Self> {
        if options.input_format == "ls" {
            return Ok(Self::Listing(Box::new(Listing::new(
                BufReader::with_capacity(256 * 1024, File::open(path)?),
                options.block_size,
                options.include_merged,
            ))));
        }
        let hex = options.input_format != "tsv";
        let reader: Box<dyn BufRead> = if hex {
            #[cfg(any(feature = "sqlite", feature = "duckdb", feature = "parquet"))]
            {
                crate::export::read_rows(path, options)?
            }
            #[cfg(not(any(feature = "sqlite", feature = "duckdb", feature = "parquet")))]
            {
                return Err("Database formats were not enabled at installation".into());
            }
        } else {
            Box::new(BufReader::new(File::open(path)?))
        };
        let mut artifact = Artifact {
            reader,
            hex,
            pending: None,
            root: None,
            diagnostics: Diagnostics::default(),
            block_size: options.block_size,
            include_merged: options.include_merged,
        };
        if !hex {
            let mut header = String::new();
            artifact.reader.read_line(&mut header)?;
            if header.trim_end_matches(['\n', '\r']) != EXPORT_COLUMNS.join("\t") {
                return Err("Invalid TSV header: expected the 12 snapshot columns".into());
            }
        }
        Ok(Self::Artifact(Box::new(artifact)))
    }

    pub fn next_section(&mut self) -> Result<Option<Section>> {
        match self {
            Self::Listing(reader) => reader.next_section(),
            Self::Artifact(reader) => reader.next_section(),
        }
    }

    pub const fn has_root(&self) -> bool {
        match self {
            Self::Listing(reader) => reader.root.is_some(),
            Self::Artifact(reader) => reader.root.is_some(),
        }
    }

    pub fn into_diagnostics(self) -> Diagnostics {
        match self {
            Self::Listing(reader) => reader.diagnostics,
            Self::Artifact(reader) => reader.diagnostics,
        }
    }
}

pub fn decode(cell: &str, hex: bool) -> Result<String> {
    if hex {
        if !cell.len().is_multiple_of(2) || !cell.is_ascii() {
            return Err("Invalid hexadecimal snapshot cell".into());
        }
        let bytes = (0..cell.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&cell[i..i + 2], 16))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        return Ok(String::from_utf8(bytes)?);
    }
    let mut value = String::new();
    let mut chars = cell.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            value.push(c);
            continue;
        }
        value.push(match chars.next() {
            Some('\\') => '\\',
            Some('t') => '\t',
            Some('n') => '\n',
            Some('r') => '\r',
            _ => return Err("Invalid TSV cell escape".into()),
        });
    }
    Ok(value)
}

pub struct Artifact {
    reader: Box<dyn BufRead>,
    hex: bool,
    pending: Option<[String; 12]>,
    root: Option<String>,
    diagnostics: Diagnostics,
    block_size: u64,
    include_merged: bool,
}

impl Artifact {
    fn row(&mut self) -> Result<Option<[String; 12]>> {
        if let Some(row) = self.pending.take() {
            return Ok(Some(row));
        }
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let cells = line
            .trim_end_matches(['\n', '\r'])
            .split('\t')
            .map(|cell| decode(cell, self.hex))
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(cells.try_into().map_err(
            |_| "Invalid snapshot row: expected 12 columns",
        )?))
    }

    fn invalid(&mut self, line: &str) {
        self.diagnostics.malformed += 1;
        if self.diagnostics.examples.len() < 3 {
            self.diagnostics.examples.push(line.to_owned());
        }
    }

    fn diagnostic(&mut self, row: &[String; 12]) -> Result<()> {
        match row[0].as_str() {
            "ls_error" => self.diagnostics.errors += 1,
            "unparsed" => self.invalid(&row[10]),
            _ => return Err(format!("Unexpected snapshot record: {}", row[0]).into()),
        }
        Ok(())
    }

    fn next_section(&mut self) -> Result<Option<Section>> {
        loop {
            let directory = loop {
                let Some(row) = self.row()? else {
                    return Ok(None);
                };
                if row[0] == "directory" {
                    break row[1].clone();
                }
                self.diagnostic(&row)?;
            };
            let root = self.root.get_or_insert_with(|| directory.clone());
            let mut section = Section {
                path: relative(root, &directory)?,
                ..Default::default()
            };
            let excluded = !self.include_merged && is_merged(&directory);
            while let Some(row) = self.row()? {
                if row[0] == "directory" {
                    self.pending = Some(row);
                    break;
                }
                if row[1] != directory {
                    return Err("Snapshot row does not match its directory header".into());
                }
                match row[0].as_str() {
                    "entry" => self.entry(&row, &mut section, excluded),
                    "total" | "total_bytes" => {
                        if let Some(blocks) = row[11].parse::<u64>().ok().and_then(|n| {
                            n.checked_mul(if row[0] == "total_bytes" {
                                1
                            } else {
                                self.block_size
                            })
                        }) {
                            section.blocks = blocks;
                            section.has_total = true;
                        } else {
                            self.invalid(&format!("total {}", row[11]));
                        }
                    }
                    _ => self.diagnostic(&row)?,
                }
            }
            if excluded {
                self.diagnostics.excluded_dirs += 1;
                continue;
            }
            if !section.has_total {
                self.diagnostics.missing_total += 1;
            }
            section.files.sort_unstable_by(|a, b| a.name.cmp(&b.name));
            if section.files.windows(2).any(|p| p[0].name == p[1].name) {
                return Err(format!("Duplicate filename in directory: {}", section.path).into());
            }
            return Ok(Some(section));
        }
    }

    fn entry(&mut self, row: &[String; 12], section: &mut Section, excluded: bool) {
        if !permissions_record(&row[2]) {
            self.invalid(&row[2..11].join(" "));
            return;
        }
        match row[2].as_bytes()[0] {
            b'-' => {
                if let Ok(size) = row[6].parse::<u64>() {
                    if excluded {
                        self.diagnostics.excluded_files += 1;
                        self.diagnostics.excluded_bytes += size;
                    } else {
                        let fields: Vec<_> = row[2..10].iter().map(String::as_str).collect();
                        section.files.push(Entry {
                            name: row[10].clone().into_boxed_str(),
                            size,
                            mtime: signature(&fields[5..8]),
                            attrs: signature(&fields[..4]),
                        });
                    }
                } else {
                    self.invalid(&row[2..11].join(" "));
                }
            }
            b'l' => section.symlinks += 1,
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decode;

    #[test]
    fn escapes_decode_once_and_invalid_cells_fail() {
        assert_eq!(decode("a\\t\\\\t\\r\\n", false).unwrap(), "a\t\\t\r\n");
        assert_eq!(decode("ed959ceab880", true).unwrap(), "한글");
        for cell in ["\\", "\\q"] {
            assert!(decode(cell, false).is_err());
        }
        for cell in ["f", "gg", "é", "ff"] {
            assert!(decode(cell, true).is_err());
        }
    }
}
