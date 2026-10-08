//! Optional file exports through installed database command-line clients.
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use super::temp::Staging;
use super::{EXPORT_COLUMNS, export_rows};
use crate::{Options, Result};

fn client(binary: &Path, init: &Path) -> Command {
    let mut command = Command::new(binary);
    command.args(["-batch", "-bail", "-init"]).arg(init);
    command
}

// Client output can be arbitrarily large on failures. Spool it to disk and
// retain only a diagnostic prefix, rather than Command::output's unbounded Vec.
fn diagnostic(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?.take(16 * 1024).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).trim().into())
}

fn execute(mut command: Command, directory: &Path) -> Result<(ExitStatus, String, String)> {
    let stdout = directory.join("client.stdout");
    let stderr = directory.join("client.stderr");
    let status = command
        .stdout(File::create(&stdout)?)
        .stderr(File::create(&stderr)?)
        .status()?;
    Ok((status, diagnostic(&stdout)?, diagnostic(&stderr)?))
}

fn check_client(binary: &Path, init: &Path, query: &str) -> Result<()> {
    let mut command = client(binary, init);
    command
        .args(["-csv", "-noheader", ":memory:", query])
        .stdin(Stdio::null());
    let (status, stdout, stderr) = execute(command, init.parent().unwrap())
        .map_err(|e| format!("Cannot execute {}: {e}", binary.display()))?;
    if !status.success() || stdout != "42" {
        return Err(format!(
            "{} capability check failed: {status} {stderr}",
            binary.display()
        )
        .into());
    }
    Ok(())
}

fn duckdb_settings(directory: &Path) -> Result<String> {
    Ok(format!(
        "SET memory_limit='64MiB'; SET threads=1; SET temp_directory={}; ",
        sql_path(directory)?
    ))
}

fn sql_path(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or("Output directory must be valid UTF-8")?;
    if text.contains('\0') {
        return Err("Output path contains a NUL character".into());
    }
    Ok(format!("'{}'", text.replace('\'', "''")))
}

fn write_cell(w: &mut impl Write, cell: &str) -> Result<()> {
    // Hex literals prevent SQL and CLI command injection and preserve raw UTF-8,
    // quotes, tabs, backslashes, and NUL bytes without TSV escape decoding.
    write!(w, "CAST(X'")?;
    for byte in cell.as_bytes() {
        write!(w, "{byte:02x}")?;
    }
    write!(w, "' AS TEXT)")?;
    Ok(())
}

pub fn export(options: &Options) -> Result<()> {
    let output = options.output.as_ref().ok_or("Missing output path")?;
    if output.try_exists()? {
        return Err(format!("Output already exists: {}", output.display()).into());
    }
    // Check input before launching tools or creating any temporary artifacts.
    let input = BufReader::new(File::open(&options.inputs[0])?);
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stage = Staging::new(&fs::canonicalize(parent)?)?;
    let init = stage.0.join("empty-init.sql");
    File::create(&init)?;
    let sqlite = options.format == "sqlite";
    let binary = if sqlite {
        &options.sqlite3_bin
    } else {
        &options.duckdb_bin
    };
    if options.format == "parquet" {
        let probe = sql_path(&stage.0.join("probe.parquet"))?;
        check_client(
            binary,
            &init,
            &format!(
                "COPY (SELECT 42 AS value) TO {probe} (FORMAT PARQUET); SELECT value FROM read_parquet({probe});"
            ),
        )?;
        return export_parquet(input, output, &stage, &init, binary);
    }
    check_client(binary, &init, "SELECT 42;")?;

    let sql = stage.0.join("export.sql");
    let mut script = BufWriter::new(File::create(&sql)?);
    write!(
        script,
        "BEGIN TRANSACTION;\nCREATE TABLE snapshot (row_index BIGINT"
    )?;
    for column in EXPORT_COLUMNS {
        write!(script, ", \"{column}\" TEXT")?;
    }
    writeln!(script, ");")?;
    let mut row_index = 0_u64;
    export_rows(input, |row| {
        row_index += 1;
        write!(script, "INSERT INTO snapshot VALUES ({row_index}")?;
        for cell in row {
            write!(script, ", ")?;
            write_cell(&mut script, cell)?;
        }
        writeln!(script, ");")?;
        Ok(())
    })?;
    writeln!(script, "COMMIT;")?;
    let database = stage.0.join("snapshot.db");
    script.flush()?;
    drop(script);
    let mut command = client(binary, &init);
    command.arg(&database).stdin(File::open(&sql)?);
    let (status, _, stderr) = execute(command, &stage.0)?;
    if !status.success() {
        return Err(format!("{} export failed: {status} {stderr}", binary.display()).into());
    }
    // Both paths are on the same filesystem. Linking publishes the finished
    // artifact atomically and refuses to overwrite a concurrently created file.
    fs::hard_link(&database, output)?;
    Ok(())
}

fn export_parquet(
    input: impl BufRead,
    output: &Path,
    stage: &Staging,
    init: &Path,
    binary: &Path,
) -> Result<()> {
    // Hex cells have no delimiters, quotes or newlines. Bulk-read once rather
    // than issuing hundreds of thousands of individually planned INSERTs.
    let rows = stage.0.join("rows.hex.tsv");
    let mut writer = BufWriter::new(File::create(&rows)?);
    let mut index = 0_u64;
    export_rows(input, |row| {
        index += 1;
        write!(writer, "{index}")?;
        for cell in row {
            write!(writer, "\t")?;
            // A prefix distinguishes an empty string from CSV NULL.
            write!(writer, "x")?;
            for byte in cell.as_bytes() {
                write!(writer, "{byte:02x}")?;
            }
        }
        writeln!(writer)?;
        Ok(())
    })?;
    writer.flush()?;
    drop(writer);
    let artifact = stage.0.join("snapshot.parquet");
    let mut columns = String::new();
    let mut projection = String::new();
    let mut empty = String::new();
    for column in EXPORT_COLUMNS {
        write!(columns, ", '{column}': 'VARCHAR'")?;
        write!(
            projection,
            ", decode(unhex(substr(\"{column}\", 2))) AS \"{column}\""
        )?;
        write!(empty, ", CAST('' AS VARCHAR) AS \"{column}\"")?;
    }
    let query = if index == 0 {
        format!("SELECT CAST(0 AS BIGINT) AS row_index{empty} WHERE FALSE")
    } else {
        format!(
            "SELECT row_index{projection} FROM read_csv({}, delim='\\t', header=false, auto_detect=false, columns={{'row_index': 'BIGINT'{columns}}}, quote='', escape='') ORDER BY row_index",
            sql_path(&rows)?
        )
    };
    let mut command = client(binary, init);
    command
        .args([
            ":memory:",
            &format!(
                "{}COPY ({query}) TO {} (FORMAT PARQUET);",
                duckdb_settings(&stage.0)?,
                sql_path(&artifact)?
            ),
        ])
        .stdin(Stdio::null());
    let (status, _, stderr) = execute(command, &stage.0)?;
    if !status.success() {
        return Err(format!("{} export failed: {status} {stderr}", binary.display()).into());
    }
    fs::hard_link(artifact, output)?;
    Ok(())
}

struct RowFile {
    reader: BufReader<File>,
    _stage: Staging,
}

impl Read for RowFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buffer)
    }
}

impl BufRead for RowFile {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.reader.fill_buf()
    }
    fn consume(&mut self, amount: usize) {
        self.reader.consume(amount);
    }
}

pub fn read_rows(path: &Path, options: &Options) -> Result<Box<dyn BufRead>> {
    let path = fs::canonicalize(path)?;
    let stage = Staging::new(&std::env::temp_dir())?;
    let init = stage.0.join("empty-init.sql");
    File::create(&init)?;
    let sqlite = options.input_format == "sqlite";
    let binary = if sqlite {
        &options.sqlite3_bin
    } else {
        &options.duckdb_bin
    };
    let table = if options.input_format == "parquet" {
        format!("read_parquet({})", sql_path(&path)?)
    } else {
        "snapshot".into()
    };
    // A hex-encoded transport keeps CLI quoting and delimiters out of cell data.
    let columns = EXPORT_COLUMNS
        .iter()
        .map(|column| format!("hex(\"{column}\")"))
        .collect::<Vec<_>>()
        .join(" || '\t' || ");
    let query = format!(
        "{}SELECT {columns} FROM {table} ORDER BY row_index;",
        if sqlite {
            String::new()
        } else {
            duckdb_settings(&stage.0)?
        }
    );
    let rows = stage.0.join("rows.hex");
    let errors = stage.0.join("stderr.txt");
    let mut command = client(binary, &init);
    command.args(["-list", "-noheader"]);
    if options.input_format == "parquet" {
        command.arg(":memory:");
    } else {
        command.arg("-readonly").arg(&path);
    }
    let status = command
        .arg(query)
        .stdin(Stdio::null())
        .stdout(File::create(&rows)?)
        .stderr(File::create(&errors)?)
        .status()
        .map_err(|e| format!("Cannot execute {}: {e}", binary.display()))?;
    if !status.success() {
        return Err(format!(
            "{} read failed: {status} {}",
            binary.display(),
            diagnostic(&errors)?
        )
        .into());
    }
    Ok(Box::new(RowFile {
        reader: BufReader::new(File::open(rows)?),
        _stage: stage,
    }))
}
