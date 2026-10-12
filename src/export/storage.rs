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

fn duckdb_settings(directory: &Path, memory_limit: u64) -> Result<String> {
    Ok(format!(
        "SET memory_limit='{memory_limit}B'; SET threads=1; SET temp_directory={}; ",
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
    let input = crate::input::open(&options.inputs[0])?;
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
        let memory_limit = options.duckdb_memory_limit_bytes();
        let probe = sql_path(&stage.0.join("probe.parquet"))?;
        let mut command = super::guard::client(binary, &init, memory_limit)?;
        command.args(["-csv", "-noheader", ":memory:", &format!(
            "{}COPY (SELECT 42 AS value) TO {probe} (FORMAT PARQUET); SELECT value FROM read_parquet({probe});",
            duckdb_settings(&stage.0, memory_limit)?
        )]).stdin(Stdio::null());
        let (status, stdout, stderr) = execute(command, &stage.0)
            .map_err(|e| format!("Cannot execute {}: {e}", binary.display()))?;
        if !status.success() || stdout != "42" {
            return Err(format!(
                "{} capability check failed: {status} {stderr}",
                binary.display()
            )
            .into());
        }
        return export_parquet(input, output, &stage, &init, options);
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
    options: &Options,
) -> Result<()> {
    let mut batches = Batches::new(stage, init, options)?;
    export_rows(input, |row| batches.row(row))?;
    if batches.count != 0 || batches.index == 0 {
        batches.flush()?;
    }
    batches.joiner.finish()?;
    fs::hard_link(stage.0.join("snapshot.parquet"), output)?;
    Ok(())
}

struct Batches<'a> {
    stage: &'a Staging,
    init: &'a Path,
    options: &'a Options,
    writer: BufWriter<File>,
    joiner: super::parquet::Joiner,
    index: u64,
    count: usize,
    bytes: usize,
}

impl<'a> Batches<'a> {
    fn new(stage: &'a Staging, init: &'a Path, options: &'a Options) -> Result<Self> {
        Ok(Self {
            stage,
            init,
            options,
            writer: BufWriter::new(File::create(stage.0.join("rows.hex.tsv"))?),
            joiner: super::parquet::Joiner::new(
                &stage.0.join("snapshot.parquet"),
                &stage.0.join("row-groups.thrift"),
            )?,
            index: 0,
            count: 0,
            bytes: 0,
        })
    }

    fn row(&mut self, row: &[&str; 12]) -> Result<()> {
        let length = 45 + row.iter().map(|cell| cell.len() * 2).sum::<usize>();
        let budget = usize::try_from(
            (self.options.duckdb_memory_limit_bytes() / 16).clamp(4096, 4 * 1024 * 1024),
        )?;
        if self.count != 0
            && (self.count == super::parquet::BATCH_ROWS || self.bytes + length > budget)
        {
            self.flush()?;
        }
        if length > 2 * crate::limits::SORT_CELL {
            return Err("Encoded export row exceeds memory limit".into());
        }
        self.index = self
            .index
            .checked_add(1)
            .filter(|&n| n <= i64::MAX as u64)
            .ok_or("Export row count overflow")?;
        write!(self.writer, "{}", self.index)?;
        for cell in row {
            write!(self.writer, "\tx")?;
            for byte in cell.as_bytes() {
                write!(self.writer, "{byte:02x}")?;
            }
        }
        writeln!(self.writer)?;
        self.count += 1;
        self.bytes += length;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        let rows = self.stage.0.join("rows.hex.tsv");
        self.writer.flush()?;
        let artifact = self.stage.0.join("batch.parquet");
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
        let query = if self.count == 0 {
            format!("SELECT CAST(0 AS BIGINT) AS row_index{empty} WHERE FALSE")
        } else {
            format!(
                "SELECT row_index{projection} FROM read_csv({}, delim='\\t', header=false, auto_detect=false, columns={{'row_index': 'BIGINT'{columns}}}, quote='', escape='') ORDER BY row_index",
                sql_path(&rows)?
            )
        };
        let mut command = super::guard::client(
            &self.options.duckdb_bin,
            self.init,
            self.options.duckdb_memory_limit_bytes(),
        )?;
        command
            .args([
                ":memory:",
                &format!(
                    "{}COPY ({query}) TO {} (FORMAT PARQUET);",
                    duckdb_settings(&self.stage.0, self.options.duckdb_memory_limit_bytes())?,
                    sql_path(&artifact)?
                ),
            ])
            .stdin(Stdio::null());
        let (status, _, stderr) = execute(command, &self.stage.0)?;
        if !status.success() {
            return Err(format!(
                "{} export failed: {status} {stderr}",
                self.options.duckdb_bin.display()
            )
            .into());
        }
        self.joiner.append(&artifact)?;
        fs::remove_file(artifact)?;
        self.writer = BufWriter::new(File::create(rows)?);
        self.count = 0;
        self.bytes = 0;
        Ok(())
    }
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
            duckdb_settings(&stage.0, options.duckdb_memory_limit_bytes())?
        }
    );
    let rows = stage.0.join("rows.hex");
    let errors = stage.0.join("stderr.txt");
    let mut command = if sqlite {
        client(binary, &init)
    } else {
        super::guard::client(binary, &init, options.duckdb_memory_limit_bytes())?
    };
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
