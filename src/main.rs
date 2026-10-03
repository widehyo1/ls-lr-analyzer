use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, BufRead, BufReader, Write};
mod cli;
mod export;
mod find;
mod input;
mod report;

use cli::{Options, options};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct Entry {
    name: Box<str>,
    size: u64,
    mtime: u64,
    attrs: u64,
}

#[derive(Debug, Default)]
struct Section {
    path: String,
    files: Vec<Entry>,
    blocks: u64,
    has_total: bool,
    symlinks: u64,
}

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

struct Listing<R> {
    reader: R,
    pending: Option<String>,
    root: Option<String>,
    line: String,
    diagnostics: Diagnostics,
    block_size: u64,
    include_merged: bool,
}

impl<R: BufRead> Listing<R> {
    fn new(reader: R, block_size: u64, include_merged: bool) -> Self {
        Self {
            reader,
            pending: None,
            root: None,
            line: String::new(),
            diagnostics: Diagnostics::default(),
            block_size,
            include_merged,
        }
    }
    fn read(&mut self) -> Result<bool> {
        self.line.clear();
        Ok(self.reader.read_line(&mut self.line)? != 0)
    }
    fn invalid(&mut self, line: &str) {
        self.diagnostics.malformed += 1;
        if self.diagnostics.examples.len() < 3 {
            self.diagnostics.examples.push(line.to_owned());
        }
    }
    fn next_section(&mut self) -> Result<Option<Section>> {
        loop {
            let raw = if let Some(p) = self.pending.take() {
                p
            } else {
                loop {
                    if !self.read()? {
                        return Ok(None);
                    }
                    let line = self.line.trim_end_matches(['\n', '\r']);
                    if let Some(p) = header(line) {
                        break p.to_owned();
                    }
                    if !line.is_empty() {
                        let line = line.to_owned();
                        if line.starts_with("ls:") {
                            self.diagnostics.errors += 1;
                        } else {
                            self.invalid(&line);
                        }
                    }
                }
            };
            let root = self.root.get_or_insert_with(|| raw.clone());
            let mut section = Section {
                path: relative(root, &raw)?,
                ..Default::default()
            };
            let excluded = !self.include_merged && is_merged(&raw);
            while self.read()? {
                let line = self.line.trim_end_matches(['\n', '\r']);
                if line.is_empty() {
                    continue;
                }
                // File records take precedence: names can themselves end in ':'.
                let file_type = line.as_bytes()[0];
                if permissions_record(line) {
                    match file_type {
                        b'-' => {
                            if let Some((f, name)) = fields(line)
                                && let Ok(size) = f[4].parse::<u64>()
                            {
                                if excluded {
                                    self.diagnostics.excluded_files += 1;
                                    self.diagnostics.excluded_bytes += size;
                                } else {
                                    section.files.push(Entry {
                                        name: name.into(),
                                        size,
                                        mtime: signature(&f[5..8]),
                                        attrs: signature(&f[..4]),
                                    });
                                }
                                continue;
                            }
                            let line = line.to_owned();
                            self.invalid(&line);
                        }
                        b'l' => section.symlinks += 1,
                        _ => (),
                    }
                } else if line.starts_with("\u{d569}\u{acc4} ") || line.starts_with("total ") {
                    if let Some(total) = line
                        .split_whitespace()
                        .nth(1)
                        .and_then(|n| n.parse::<u64>().ok())
                        .and_then(|n| n.checked_mul(self.block_size))
                    {
                        section.blocks = total;
                        section.has_total = true;
                    } else {
                        let line = line.to_owned();
                        self.invalid(&line);
                    }
                } else if let Some(p) = header(line) {
                    self.pending = Some(p.to_owned());
                    break;
                } else if line.starts_with("ls:") {
                    self.diagnostics.errors += 1;
                } else {
                    let line = line.to_owned();
                    self.invalid(&line);
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
                export::export_tsv(BufReader::new(File::open(&o.inputs[0])?), &mut out)?;
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
