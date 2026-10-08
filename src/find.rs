//! Decode saved GNU find snapshots; never inspect the collection filesystem.
use crate::export::{EXPORT_COLUMNS, temp::Staging, tsv_row};
use crate::sort::{self, Record, Sorter};
use crate::{Result, permissions_record};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
type Row = [String; 12];
pub fn snapshot(path: &Path) -> Result<Staging> {
    let input = crate::input::open(path)?;
    let stage = Staging::new(&std::env::temp_dir())?;
    let mut writer = BufWriter::new(File::create(stage.0.join("snapshot.tsv"))?);
    normalize(input, &mut writer)?;
    writer.flush()?;
    Ok(stage)
}

pub fn normalize(mut reader: impl BufRead, writer: &mut impl Write) -> Result<()> {
    let mut paths = Sorter::new()?;
    let mut ordinal = 0_u64;
    while let Some(cells) = read_record(&mut reader)? {
        validate(&cells)?;
        if ordinal == 0 && (!cells[0].is_empty() || cells[1] != "d") {
            return Err("Find root must be the first directory record".into());
        }
        paths.push(Record {
            key: (cells[0].clone(), format!("{ordinal:020}")),
            data: cells.join("\0").into_bytes(),
        })?;
        ordinal = ordinal.checked_add(1).ok_or("find record count overflow")?;
    }
    if ordinal == 0 {
        return Err("Missing find root record".into());
    }
    let mut paths = paths.finish()?;
    let mut groups = Sorter::new()?;
    let mut previous = None;
    while let Some(record) = paths.next()? {
        if previous.as_ref() == Some(&record.key.0) {
            return Err("Duplicate find path".into());
        }
        previous = Some(record.key.0.clone());
        let text = String::from_utf8(record.data)?;
        let cells: Vec<_> = text.split('\0').collect();
        let path = &record.key.0;
        if cells[1] == "d" {
            groups.push(Record {
                key: (path.clone(), String::new()),
                data: Vec::new(),
            })?;
        }
        if path.is_empty() {
            continue;
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let blocks = cells[3]
            .parse::<u64>()?
            .checked_mul(512)
            .ok_or("find block overflow")?;
        let mut row: Row = std::array::from_fn(|_| String::new());
        row[0] = "entry".into();
        row[1] = snapshot_directory(parent);
        for (to, from) in [(2, 4), (3, 5), (4, 6), (5, 7), (6, 2)] {
            row[to] = cells[from].into();
        }
        row[7] = "epoch".into();
        row[8] = cells[8].into();
        row[9] = "find".into();
        row[10] = if cells[1] == "l" {
            format!("{name} -> {}", cells[9])
        } else {
            name.into()
        };
        let mut data = blocks.to_le_bytes().to_vec();
        data.extend_from_slice(row.join("\0").as_bytes());
        groups.push(Record {
            key: (parent.into(), record.key.1),
            data,
        })?;
    }
    drop(paths);
    let mut groups = groups.finish()?;
    // First pass validates directory existence and reduces direct-child totals.
    // A second pass emits totals before entries without buffering a directory.
    let stage = Staging::new(&std::env::temp_dir())?;
    let totals_path = stage.0.join("totals");
    let mut totals = BufWriter::new(File::create(&totals_path)?);
    let mut current = None;
    let mut total = 0_u64;
    while let Some(record) = groups.next()? {
        if current.as_ref() != Some(&record.key.0) {
            if let Some(path) = current.take() {
                sort::write_record(
                    &mut totals,
                    &Record {
                        key: (path, String::new()),
                        data: total.to_le_bytes().to_vec(),
                    },
                )?;
            }
            if !record.key.1.is_empty() {
                return Err("Missing find directory record".into());
            }
            current = Some(record.key.0);
            total = 0;
        } else {
            let blocks = u64::from_le_bytes(record.data[..8].try_into()?);
            total = total.checked_add(blocks).ok_or("find total overflow")?;
        }
    }
    if let Some(path) = current {
        sort::write_record(
            &mut totals,
            &Record {
                key: (path, String::new()),
                data: total.to_le_bytes().to_vec(),
            },
        )?;
    }
    totals.flush()?;
    drop(totals);
    let mut totals = BufReader::new(File::open(totals_path)?);
    groups.rewind()?;
    tsv_row(writer, &EXPORT_COLUMNS)?;
    while let Some(record) = groups.next()? {
        if record.key.1.is_empty() {
            let total =
                sort::read_record(&mut totals)?.ok_or("Missing temporary directory total")?;
            let path = snapshot_directory(&record.key.0);
            let mut header = [""; 12];
            header[0] = "directory";
            header[1] = &path;
            tsv_row(writer, &header)?;
            header[0] = "total_bytes";
            let bytes = u64::from_le_bytes(total.data[..8].try_into()?).to_string();
            header[11] = &bytes;
            tsv_row(writer, &header)?;
        } else {
            let text = std::str::from_utf8(&record.data[8..])?;
            let cells: Vec<_> = text.split('\0').collect();
            tsv_row(
                writer,
                &cells.try_into().map_err(|_| "Invalid temporary find row")?,
            )?;
        }
    }
    Ok(())
}

fn read_record(reader: &mut impl BufRead) -> Result<Option<Vec<String>>> {
    let mut cells = Vec::with_capacity(10);
    for index in 0..10 {
        let mut bytes = Vec::new();
        if reader.read_until(0, &mut bytes)? == 0 {
            if index == 0 {
                return Ok(None);
            }
            return Err("Truncated find record".into());
        }
        if bytes.pop() != Some(0) {
            return Err("Unterminated find field".into());
        }
        cells.push(String::from_utf8(bytes).map_err(|_| "find paths and metadata must be UTF-8")?);
    }
    Ok(Some(cells))
}

fn validate(cells: &[String]) -> Result<()> {
    let path = &cells[0];
    if !path.is_empty() && path.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return Err("Find paths must be relative to one root".into());
    }
    let kind = match cells[1].as_str() {
        "f" => '-',
        "d" => 'd',
        "l" => 'l',
        "b" => 'b',
        "c" => 'c',
        "p" => 'p',
        "s" => 's',
        _ => return Err("Invalid find entry type".into()),
    };
    if cells[4].len() != 10 || !permissions_record(&cells[4]) || !cells[4].starts_with(kind) {
        return Err("Invalid find permissions or type mismatch".into());
    }
    for index in [2, 3, 5, 6, 7] {
        cells[index].parse::<u64>()?;
    }
    let stamp = cells[8].strip_prefix('-').unwrap_or(&cells[8]);
    let Some((seconds, fraction)) = stamp.split_once('.') else {
        return Err("Invalid find epoch timestamp".into());
    };
    if seconds.is_empty()
        || fraction.is_empty()
        || !seconds.bytes().all(|c| c.is_ascii_digit())
        || !fraction.bytes().all(|c| c.is_ascii_digit())
    {
        return Err("Invalid find epoch timestamp".into());
    }
    Ok(())
}

fn snapshot_directory(path: &str) -> String {
    if path.is_empty() {
        ".".into()
    } else {
        format!("./{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_streams_fail() {
        for raw in [b"".as_slice(), b"unterminated", b"file\0f\0"] {
            assert!(normalize(raw, &mut Vec::new()).is_err());
        }
    }

    fn record(path: &str, kind: &str, mode: &str, size: &str) -> String {
        [
            path,
            kind,
            size,
            "0",
            mode,
            "1",
            "1000",
            "1000",
            "1.0000000000",
            "",
        ]
        .join("\0")
            + "\0"
    }

    #[test]
    fn invalid_fields_duplicate_paths_and_missing_parents_fail() {
        let root = record("", "d", "drwxr-xr-x", "4096");
        for invalid in [
            record("../escape", "f", "-rw-r--r--", "1"),
            record("a", "f", "drwxr-xr-x", "1"),
            record("a", "f", "-rw-r--r--", "NaN"),
            record("missing/a", "f", "-rw-r--r--", "1"),
            root.clone(),
        ] {
            assert!(normalize((root.clone() + &invalid).as_bytes(), &mut Vec::new()).is_err());
        }
    }

    #[test]
    fn whitespace_names_and_symlink_targets_survive_normalization() {
        let root = record("", "d", "drwxr-xr-x", "4096");
        let file = record("한글\tline\n\\name", "f", "-rw-r--r--", "0");
        let link = [
            "link",
            "l",
            "4",
            "0",
            "lrwxrwxrwx",
            "1",
            "1000",
            "1000",
            "1.0000000000",
            "target\t\n\\",
        ]
        .join("\0")
            + "\0";
        let mut output = Vec::new();
        normalize((root + &file + &link).as_bytes(), &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("한글\\tline\\n\\\\name"));
        assert!(text.contains("link -> target\\t\\n\\\\"));
        assert!(text.lines().all(|line| line.split('\t').count() == 12));
    }
}
