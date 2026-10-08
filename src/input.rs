//! Stream every input format into a disk-sorted directory/file snapshot.
use crate::export::EXPORT_COLUMNS;
use crate::sort::{Record, Sorted, Sorter};
use crate::{Diagnostics, Options, Result, is_merged, permissions_record, relative, signature};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

pub fn is_stdin(path: &Path) -> bool {
    path == Path::new("-")
}

/// Text and raw find inputs share a buffered reader without staging stdin.
pub fn open(path: &Path) -> Result<Box<dyn BufRead>> {
    if is_stdin(path) {
        Ok(Box::new(std::io::stdin().lock()))
    } else {
        Ok(Box::new(BufReader::with_capacity(
            256 * 1024,
            File::open(path)?,
        )))
    }
}

pub fn numbers<const N: usize>(data: &[u8]) -> Result<[u64; N]> {
    if data.len() != N * 8 {
        return Err("Invalid temporary numeric record".into());
    }
    let mut result = [0; N];
    for (number, bytes) in result.iter_mut().zip(data.chunks_exact(8)) {
        *number = u64::from_le_bytes(bytes.try_into()?);
    }
    Ok(result)
}

pub fn encode(numbers: &[u64]) -> Vec<u8> {
    numbers.iter().flat_map(|n| n.to_le_bytes()).collect()
}

pub struct Snapshot {
    rows: Sorted,
    previous: Option<(String, String)>,
    pub diagnostics: Diagnostics,
}

impl Snapshot {
    pub fn next(&mut self) -> Result<Option<Record>> {
        let row = self.rows.next()?;
        if let Some(row) = &row {
            if self.previous.as_ref() == Some(&row.key) {
                let kind = if row.key.1 == "0" {
                    "directory"
                } else {
                    "filename in directory"
                };
                return Err(format!("Duplicate {kind}: {}", row.key.0).into());
            }
            self.previous = Some(row.key.clone());
        }
        Ok(row)
    }
}

#[derive(Default)]
struct Directory {
    raw: String,
    path: String,
    blocks: u64,
    links: u64,
    order: u64,
    has_total: bool,
    excluded: bool,
}

struct Scan<'a> {
    sorter: Sorter,
    root: Option<String>,
    directory: Option<Directory>,
    order: u64,
    diagnostics: Diagnostics,
    options: &'a Options,
}

impl Scan<'_> {
    fn invalid(&mut self, line: &str) {
        self.diagnostics.malformed += 1;
        if self.diagnostics.examples.len() < 3 {
            self.diagnostics.examples.push(line.into());
        }
    }

    fn finish_directory(&mut self) -> Result<()> {
        if let Some(directory) = self.directory.take() {
            if directory.excluded {
                self.diagnostics.excluded_dirs += 1;
            } else {
                if !directory.has_total {
                    self.diagnostics.missing_total += 1;
                }
                self.sorter.push(Record {
                    key: (directory.path, "0".into()),
                    data: encode(&[directory.blocks, directory.links, directory.order]),
                })?;
            }
        }
        Ok(())
    }

    fn row(&mut self, row: &[&str; 12]) -> Result<()> {
        if row[0] == "directory" {
            self.finish_directory()?;
            let root = self.root.get_or_insert_with(|| row[1].into());
            self.directory = Some(Directory {
                raw: row[1].into(),
                path: relative(root, row[1])?,
                excluded: !self.options.include_merged && is_merged(row[1]),
                order: self.order,
                ..Default::default()
            });
            self.order = self
                .order
                .checked_add(1)
                .ok_or("Directory count overflow")?;
            return Ok(());
        }
        if self.directory.as_ref().is_some_and(|d| d.raw != row[1]) {
            return Err("Snapshot row does not match its directory header".into());
        }
        match row[0] {
            "ls_error" => self.diagnostics.errors += 1,
            "unparsed" => self.invalid(row[10]),
            "total" | "total_bytes" => {
                let blocks = row[11].parse::<u64>().ok().and_then(|n| {
                    n.checked_mul(if row[0] == "total_bytes" {
                        1
                    } else {
                        self.options.block_size
                    })
                });
                if let Some(blocks) = blocks {
                    if let Some(directory) = &mut self.directory {
                        directory.blocks = blocks;
                        directory.has_total = true;
                    }
                } else {
                    self.invalid(&format!("total {}", row[11]));
                }
            }
            "entry" => {
                if self.directory.is_none() {
                    return Err("Entry before directory header".into());
                }
                if !permissions_record(row[2]) {
                    self.invalid(&row[2..11].join(" "));
                    return Ok(());
                }
                match row[2].as_bytes()[0] {
                    b'-' => {
                        let Ok(size) = row[6].parse::<u64>() else {
                            self.invalid(&row[2..11].join(" "));
                            return Ok(());
                        };
                        let directory = self.directory.as_ref().unwrap();
                        if directory.excluded {
                            self.diagnostics.excluded_files += 1;
                            self.diagnostics.excluded_bytes = self
                                .diagnostics
                                .excluded_bytes
                                .checked_add(size)
                                .ok_or("Excluded size overflow")?;
                        } else {
                            self.sorter.push(Record {
                                key: (directory.path.clone(), format!("1{}", row[10])),
                                data: encode(&[
                                    size,
                                    signature(&row[7..10]),
                                    signature(&row[2..6]),
                                ]),
                            })?;
                        }
                    }
                    b'l' => self.directory.as_mut().unwrap().links += 1,
                    _ => (),
                }
            }
            _ => return Err(format!("Unexpected snapshot record: {}", row[0]).into()),
        }
        Ok(())
    }
}

pub fn snapshot(path: &Path, options: &Options) -> Result<Snapshot> {
    let hex = !matches!(options.input_format.as_str(), "ls" | "tsv");
    let reader = if hex {
        #[cfg(any(feature = "sqlite", feature = "duckdb", feature = "parquet"))]
        {
            crate::export::read_rows(path, options)?
        }
        #[cfg(not(any(feature = "sqlite", feature = "duckdb", feature = "parquet")))]
        {
            return Err("Database formats were not enabled at installation".into());
        }
    } else {
        open(path)?
    };
    snapshot_reader(reader, options, hex)
}

fn snapshot_reader(mut reader: impl BufRead, options: &Options, hex: bool) -> Result<Snapshot> {
    let mut scan = Scan {
        sorter: Sorter::new()?,
        root: None,
        directory: None,
        order: 0,
        diagnostics: Diagnostics::default(),
        options,
    };
    if options.input_format == "ls" {
        crate::export::rows(reader, |row| scan.row(row), true)?;
    } else {
        let mut line = String::new();
        if !hex {
            reader.read_line(&mut line)?;
            if line.trim_end_matches(['\n', '\r']) != EXPORT_COLUMNS.join("\t") {
                return Err("Invalid TSV header: expected the 12 snapshot columns".into());
            }
        }
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            let cells = line
                .trim_end_matches(['\n', '\r'])
                .split('\t')
                .map(|cell| decode(cell, hex))
                .collect::<Result<Vec<_>>>()?;
            let row: [&str; 12] = cells
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .try_into()
                .map_err(|_| "Invalid snapshot row: expected 12 columns")?;
            scan.row(&row)?;
        }
    }
    scan.finish_directory()?;
    if scan.root.is_none() {
        return Err("No directory headers in input. Expected ls -lR output.".into());
    }
    Ok(Snapshot {
        rows: scan.sorter.finish()?,
        previous: None,
        diagnostics: scan.diagnostics,
    })
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
