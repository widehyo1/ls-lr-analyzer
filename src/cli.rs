use std::path::PathBuf;

use crate::Result;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Legacy,
    Export,
    Report,
}

pub struct Options {
    pub inputs: Vec<PathBuf>,
    pub depth: usize,
    pub top: usize,
    pub block_size: u64,
    pub include_merged: bool,
    pub days: Option<f64>,
    pub format: String,
    pub input_format: String,
    pub output: Option<PathBuf>,
    pub sqlite3_bin: PathBuf,
    pub duckdb_bin: PathBuf,
    pub mode: Mode,
    pub source: String,
}

const HELP: &str = "ls-lr-analyzer [OPTIONS] [BEFORE] AFTER
ls-lr-analyzer export [OPTIONS] SNAPSHOT
ls-lr-analyzer report [OPTIONS] [BEFORE] AFTER

export: normalize saved ls/find metadata. report: analyze one snapshot or compare two.
Without a subcommand, the original listing report interface is retained.
Use - for stdin (ls/find/tsv input); at most one comparison input may be stdin.

--format FORMAT       Export: tsv (default), sqlite, parquet
--input-format FORMAT ls (default), find, tsv, sqlite, duckdb, parquet
--output FILE         Export destination; TSV defaults to stdout
--sqlite3-bin PATH    SQLite CLI executable (default: sqlite3 on PATH)
--duckdb-bin PATH     DuckDB CLI for DuckDB/Parquet (default: duckdb on PATH)
--source SOURCE       ls (default) or find; read snapshot files or stdin
--depth N             Report grouping depth (default: 1)
--top N               Report ranking limit (default: 10)
--days N              Report comparison interval; show average net growth
--block-size N        Report bytes per ls total unit (default: 1024)
--include-merged      Report includes container overlay merged views
-V, --version         Print version
-h, --help            Print usage
--                    Treat remaining arguments as input paths

Analyze regular-file sizes and directory totals from saved ls/find snapshots.
Not a replacement for df/du or a content integrity check.";

pub fn options() -> Result<Option<Options>> {
    let mut args = std::env::args().skip(1).peekable();
    let mode = match args.peek().map(String::as_str) {
        Some("export") => {
            args.next();
            Mode::Export
        }
        Some("report") => {
            args.next();
            Mode::Report
        }
        _ => Mode::Legacy,
    };
    let mut o = Options {
        inputs: Vec::new(),
        depth: 1,
        top: 10,
        block_size: 1024,
        include_merged: false,
        days: None,
        format: if mode == Mode::Export { "tsv" } else { "text" }.into(),
        input_format: "ls".into(),
        output: None,
        sqlite3_bin: "sqlite3".into(),
        duckdb_bin: "duckdb".into(),
        mode,
        source: "ls".into(),
    };
    let mut positional = false;
    let mut report_options = false;
    while let Some(arg) = args.next() {
        if positional {
            o.inputs.push(arg.into());
            continue;
        }
        match arg.as_str() {
            "-" => o.inputs.push(arg.into()),
            "--" => positional = true,
            "--source" => o.source = args.next().ok_or("--source requires a value")?,
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("ls-lr-analyzer {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--depth" => {
                o.depth = args.next().ok_or("--depth requires a value")?.parse()?;
                report_options = true;
            }
            "--top" => {
                o.top = args.next().ok_or("--top requires a value")?.parse()?;
                report_options = true;
            }
            "--block-size" => {
                o.block_size = args
                    .next()
                    .ok_or("--block-size requires a value")?
                    .parse()?;
                report_options = true;
            }
            "--days" => {
                o.days = Some(args.next().ok_or("--days requires a value")?.parse()?);
                report_options = true;
            }
            "--include-merged" => {
                o.include_merged = true;
                report_options = true;
            }
            "--format" => {
                if mode == Mode::Report {
                    return Err("report uses --input-format; --format is for export".into());
                }
                o.format = args.next().ok_or("--format requires a value")?;
            }
            "--input-format" => {
                o.input_format = args.next().ok_or("--input-format requires a value")?;
            }
            "--output" => o.output = Some(args.next().ok_or("--output requires a path")?.into()),
            "--sqlite3-bin" => {
                o.sqlite3_bin = args.next().ok_or("--sqlite3-bin requires a path")?.into();
            }
            "--duckdb-bin" => {
                o.duckdb_bin = args.next().ok_or("--duckdb-bin requires a path")?.into();
            }
            _ if arg.starts_with('-') => return Err(format!("Unknown option: {arg}").into()),
            _ => o.inputs.push(arg.into()),
        }
    }
    if mode == Mode::Export && report_options {
        return Err("Report options cannot be used with export".into());
    }
    validate(&o)?;
    Ok(Some(o))
}

fn check_feature(format: &str) -> Result<()> {
    let enabled = matches!(format, "ls" | "find" | "text" | "tsv")
        || (format == "sqlite" && cfg!(feature = "sqlite"))
        || (format == "duckdb" && cfg!(feature = "duckdb"))
        || (format == "parquet" && cfg!(feature = "parquet"));
    if !enabled {
        return Err(
            format!("{format} format is not enabled; reinstall with --features {format}").into(),
        );
    }
    Ok(())
}

fn validate(o: &Options) -> Result<()> {
    if !matches!(o.source.as_str(), "ls" | "find") {
        return Err("--source requires ls or find".into());
    }
    if o.source == "find" && !matches!(o.input_format.as_str(), "ls" | "find") {
        return Err("--source find cannot be combined with another input format".into());
    }
    if !(1..=2).contains(&o.inputs.len()) {
        return Err("Expected AFTER or BEFORE AFTER. See --help.".into());
    }
    let stdin_count = o
        .inputs
        .iter()
        .filter(|p| crate::input::is_stdin(p))
        .count();
    if stdin_count > 1 {
        return Err("Only one input may read stdin; - - is not supported".into());
    }
    if stdin_count != 0 && !matches!(o.input_format.as_str(), "ls" | "find" | "tsv") {
        return Err(
            "stdin supports ls, find, and tsv input; database/Parquet input requires a file path"
                .into(),
        );
    }
    if o.depth == 0 || o.top == 0 || o.block_size == 0 {
        return Err("depth, top, and block-size must be positive".into());
    }
    if o.days.is_some_and(|n| !n.is_finite() || n <= 0.0) {
        return Err("--days must be finite and positive".into());
    }
    if o.inputs.len() == 1 && o.days.is_some() {
        return Err("--days requires two input files".into());
    }
    if !matches!(o.format.as_str(), "text" | "tsv" | "sqlite" | "parquet") {
        return Err("--format requires text, tsv, sqlite, or parquet; DuckDB file export is no longer supported".into());
    }
    if !matches!(
        o.input_format.as_str(),
        "ls" | "find" | "tsv" | "sqlite" | "duckdb" | "parquet"
    ) {
        return Err("--input-format requires ls, find, tsv, sqlite, duckdb, or parquet".into());
    }
    if o.mode == Mode::Export && o.format == "text" {
        return Err("export requires tsv, sqlite, or parquet".into());
    }
    if o.format != "text" && o.inputs.len() != 1 {
        return Err(format!("--format {} requires one input file", o.format).into());
    }
    if matches!(o.format.as_str(), "sqlite" | "parquet") && o.output.is_none() {
        return Err("--output FILE is required for sqlite and parquet".into());
    }
    if o.format == "text" && o.output.is_some() {
        return Err("Redirect stdout to save a report; --output is for export".into());
    }
    if o.format != "text" && !matches!(o.input_format.as_str(), "ls" | "find") {
        return Err("export requires an ls or find snapshot input".into());
    }
    check_feature(&o.format)?;
    check_feature(&o.input_format)
}
