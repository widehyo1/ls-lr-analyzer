//! Shared snapshot rows, TSV serialization, and optional file backends.
use std::fs::{self, File};
use std::io::{BufRead, BufWriter, Write};
use std::path::Path;

use crate::{Result, fields, header, permissions_record};

pub mod temp;

#[cfg(any(feature = "sqlite", feature = "duckdb", feature = "parquet"))]
mod storage;
#[cfg(any(feature = "sqlite", feature = "duckdb", feature = "parquet"))]
pub use storage::export as export_file;
#[cfg(any(feature = "sqlite", feature = "duckdb", feature = "parquet"))]
pub use storage::read_rows;

// TSV cells use reversible escapes, keeping every record on one physical line.
pub fn tsv_row(w: &mut impl Write, cells: &[&str; 12]) -> Result<()> {
    for (i, cell) in cells.iter().enumerate() {
        if i != 0 {
            write!(w, "\t")?;
        }
        for c in cell.chars() {
            match c {
                '\\' => write!(w, "\\\\")?,
                '\t' => write!(w, "\\t")?,
                '\n' => write!(w, "\\n")?,
                '\r' => write!(w, "\\r")?,
                _ => write!(w, "{c}")?,
            }
        }
    }
    writeln!(w)?;
    Ok(())
}

pub const EXPORT_COLUMNS: [&str; 12] = [
    "record",
    "directory",
    "permissions",
    "links",
    "owner",
    "group",
    "size_or_device",
    "month",
    "day",
    "time_or_year",
    "name",
    "total",
];

pub fn export_tsv(reader: impl BufRead, w: &mut impl Write) -> Result<()> {
    tsv_row(w, &EXPORT_COLUMNS)?;
    export_rows(reader, |row| tsv_row(w, row))
}

pub fn export_tsv_file(input: &Path, output: &Path) -> Result<()> {
    if output.try_exists()? {
        return Err(format!("Output already exists: {}", output.display()).into());
    }
    let reader = crate::input::open(input)?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stage = temp::Staging::new(&fs::canonicalize(parent)?)?;
    let artifact = stage.0.join("snapshot.tsv");
    let mut writer = BufWriter::new(File::create(&artifact)?);
    export_tsv(reader, &mut writer)?;
    writer.flush()?;
    drop(writer);
    fs::hard_link(artifact, output)?;
    Ok(())
}

pub(crate) fn export_rows(
    reader: impl BufRead,
    emit: impl FnMut(&[&str; 12]) -> Result<()>,
) -> Result<()> {
    rows(reader, emit, false)
}

pub(crate) fn rows(
    mut reader: impl BufRead,
    mut emit: impl FnMut(&[&str; 12]) -> Result<()>,
    report: bool,
) -> Result<()> {
    if reader
        .fill_buf()?
        .starts_with(b"record\tdirectory\tpermissions\t")
    {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        if header.trim_end() != EXPORT_COLUMNS.join("\t") {
            return Err("Invalid snapshot TSV header".into());
        }
        for line in reader.lines() {
            let line = line?;
            let cells = line
                .split('\t')
                .map(|v| crate::input::decode(v, false))
                .collect::<Result<Vec<_>>>()?;
            let row: Vec<_> = cells.iter().map(String::as_str).collect();
            emit(&row.try_into().map_err(|_| "Invalid snapshot TSV row")?)?;
        }
        return Ok(());
    }
    let mut directory = String::new();
    let mut warnings = 0;
    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let mut row = [""; 12];
        // A device's major/minor pair occupies two whitespace-delimited tokens.
        let device_line;
        let record_line = if permissions_record(&line) && matches!(line.as_bytes()[0], b'b' | b'c')
        {
            device_line = line.find(',').map_or_else(
                || line.clone(),
                |comma| format!("{}{}", &line[..=comma], line[comma + 1..].trim_start()),
            );
            device_line.as_str()
        } else {
            line.as_str()
        };
        if permissions_record(&line) {
            if let Some((metadata, name)) = fields(record_line) {
                row[0] = "entry";
                row[2..10].copy_from_slice(&metadata);
                row[10] = name;
            } else {
                row[0] = "unparsed";
                row[10] = &line;
                warnings += 1;
            }
        } else if line.starts_with("total ") || line.starts_with("합계 ") {
            if let Some(total) = line.split_whitespace().nth(1)
                && total.parse::<u64>().is_ok()
                && (report || line.split_whitespace().count() == 2)
            {
                row[0] = "total";
                row[11] = total;
            } else {
                row[0] = "unparsed";
                row[10] = &line;
                warnings += 1;
            }
        } else if line.starts_with("ls:") {
            row[0] = "ls_error";
            row[10] = &line;
            warnings += 1;
        } else if let Some(path) = header(&line) {
            path.clone_into(&mut directory);
            row[0] = "directory";
        } else {
            row[0] = "unparsed";
            row[10] = &line;
            warnings += 1;
        }
        row[1] = &directory;
        emit(&row)?;
    }
    if warnings != 0 && !report {
        eprintln!("Warning: {warnings} ls error or unparsed lines preserved in export");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    #[test]
    fn tsv_preserves_metadata_special_entries_and_escaped_names() {
        let input = "/data:\n합계 4\n-rw-r--r-- 2 user group 123 Oct 1 12:00 한글\tfile\\t:\nlrwxrwxrwx 1 u g 6 Sep 28 2025 link -> target\ncrw-rw-rw- 1 root root 1,   3 Oct 1 12:00 null\ndrwxr-xr-x 2 u g 4096 Oct 1 12:00 empty\n/data/empty:\ntotal 0\n/data/containers/storage/overlay/id/merged:\ntotal 0\nls: permission denied\nbad input\n";
        let mut out = Vec::new();
        export_tsv(io::Cursor::new(input), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let rows: Vec<Vec<&str>> = text.lines().map(|l| l.split('\t').collect()).collect();
        assert!(rows.iter().all(|r| r.len() == 12));
        assert_eq!(
            rows[3],
            [
                "entry",
                "/data",
                "-rw-r--r--",
                "2",
                "user",
                "group",
                "123",
                "Oct",
                "1",
                "12:00",
                "한글\\tfile\\\\t:",
                ""
            ]
        );
        assert_eq!(rows[4][10], "link -> target");
        assert_eq!(rows[5][6], "1,3");
        assert_eq!(rows[6][2], "drwxr-xr-x");
        assert_eq!(rows[7][1], "/data/empty");
        assert_eq!(rows[8][11], "0");
        assert_eq!(rows[9][1], "/data/containers/storage/overlay/id/merged");
        assert_eq!(rows[11][0], "ls_error");
        assert_eq!(rows[12][0], "unparsed");
    }
}
