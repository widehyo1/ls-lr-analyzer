use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Write};
mod cli;
mod export;
mod find;
mod input;
mod report;
mod sort;
mod top;

use cli::{Options, options};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Default)]
struct Diagnostics {
    excluded_dirs: u64,
    excluded_files: u64,
    excluded_bytes: u64,
    missing_total: u64,
    malformed: u64,
    errors: u64,
    examples: Vec<String>,
}

fn signature(tokens: &[&str]) -> u64 {
    let mut h = DefaultHasher::new();
    tokens.hash(&mut h);
    h.finish()
}

// Split eight metadata fields, preserving spaces in the remaining filename.
fn fields(line: &str) -> Option<([&str; 8], &str)> {
    let mut metadata = [""; 8];
    let mut rest = line;
    for item in &mut metadata {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace)?;
        *item = &rest[..end];
        rest = &rest[end..];
    }
    rest = rest.trim_start();
    (!rest.is_empty()).then_some((metadata, rest))
}

fn header(line: &str) -> Option<&str> {
    if line.starts_with("ls:")
        || line.starts_with("\u{d569}\u{acc4} ")
        || line.starts_with("total ")
    {
        return None;
    }
    line.strip_suffix(':')
}

fn permissions_record(line: &str) -> bool {
    line.split_whitespace().next().is_some_and(|p| {
        let bytes = p.as_bytes();
        bytes.len() >= 10
            && matches!(bytes[0], b'-' | b'd' | b'l' | b'b' | b'c' | b'p' | b's')
            && bytes[1..10]
                .iter()
                .all(|b| matches!(b, b'r' | b'w' | b'x' | b's' | b'S' | b't' | b'T' | b'-'))
    })
}

fn relative(root: &str, path: &str) -> Result<String> {
    if path == root {
        return Ok(".".to_owned());
    }
    let prefix = format!("{}/", root.trim_end_matches('/'));
    if let Some(p) = path.strip_prefix(&prefix) {
        return Ok(p.to_owned());
    }
    if root == "." && path.starts_with("./") {
        return Ok(path[2..].to_owned());
    }
    Err(format!("Listing is not under a single root: root={root:?}, directory={path:?}").into())
}

fn is_merged(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    parts
        .windows(5)
        .any(|p| p[0] == "containers" && p[1] == "storage" && p[2] == "overlay" && p[4] == "merged")
}

fn run() -> Result<()> {
    if let Some(mut o) = options()? {
        let started = std::time::Instant::now();
        if let Some(path) = &o.output {
            eprintln!("started: export {} -> {}", o.format, path.display());
        }
        let original_inputs = o.inputs.clone();
        let snapshots = if o.source == "find" || o.input_format == "find" {
            o.inputs
                .iter()
                .map(|path| find::snapshot(path))
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        if !snapshots.is_empty() {
            o.inputs = snapshots
                .iter()
                .map(|stage| stage.0.join("snapshot.tsv"))
                .collect();
            if o.format == "text" {
                o.input_format = "tsv".into();
            }
        }
        let stdout = io::stdout();
        let mut out = io::BufWriter::new(stdout.lock());
        if matches!(o.format.as_str(), "sqlite" | "parquet") {
            #[cfg(any(feature = "sqlite", feature = "duckdb", feature = "parquet"))]
            export::export_file(&o)?;
        } else if o.format == "tsv" {
            if let Some(path) = &o.output {
                export::export_tsv_file(&o.inputs[0], path)?;
            } else {
                export::export_tsv(input::open(&o.inputs[0])?, &mut out)?;
            }
        } else {
            let a = report::analyze(&o)?;
            if !snapshots.is_empty() {
                o.inputs = original_inputs;
            }
            report::render(&mut out, &o, &a)?;
        }
        out.flush()?;
        if let Some(path) = &o.output {
            eprintln!(
                "finished: export {} -> {} ({:.3}s)",
                o.format,
                path.display(),
                started.elapsed().as_secs_f64()
            );
        }
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        if e.downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
        {
            return;
        }
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
