use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Options {
    inputs: Vec<PathBuf>,
    depth: usize,
    top: usize,
    block_size: u64,
    include_merged: bool,
    days: Option<f64>,
}

fn options() -> Result<Option<Options>> {
    let mut o = Options {
        inputs: Vec::new(),
        depth: 1,
        top: 10,
        block_size: 1024,
        include_merged: false,
        days: None,
    };
    let mut args = std::env::args().skip(1);
    let mut positional = false;
    while let Some(arg) = args.next() {
        if positional {
            o.inputs.push(arg.into());
            continue;
        }
        match arg.as_str() {
            "--" => positional = true,
            "-h" | "--help" => {
                println!(
                    "ls-lr-analyzer [OPTIONS] [BEFORE] AFTER\n\n\
                    One input: capacity report. Two inputs: additions, removals, and growth.\n\n\
                    --depth N         Path grouping depth (default: 1)\n\
                    --top N           Maximum entries per ranking (default: 10)\n\
                    --days N          Comparison interval in days; show average net growth\n\
                    --block-size N    Bytes per ls total unit (default: 1024)\n\
                    --include-merged  Include container overlay merged views\n\
                    --                Treat remaining arguments as input paths\n\n\
                    Analyze regular-file sizes and directory totals from GNU ls -lR.\n\
                    Not a replacement for df/du or a content integrity check."
                );
                return Ok(None);
            }
            "--depth" => o.depth = args.next().ok_or("--depth requires a value")?.parse()?,
            "--top" => o.top = args.next().ok_or("--top requires a value")?.parse()?,
            "--block-size" => {
                o.block_size = args
                    .next()
                    .ok_or("--block-size requires a value")?
                    .parse()?
            }
            "--days" => o.days = Some(args.next().ok_or("--days requires a value")?.parse()?),
            "--include-merged" => o.include_merged = true,
            _ if arg.starts_with('-') => return Err(format!("Unknown option: {arg}").into()),
            _ => o.inputs.push(arg.into()),
        }
    }
    if !(1..=2).contains(&o.inputs.len()) {
        return Err("Expected AFTER or BEFORE AFTER. See --help.".into());
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
    Ok(Some(o))
}

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

#[derive(Default, Clone, Debug)]
struct Summary {
    old_bytes: u64,
    new_bytes: u64,
    old_blocks: u64,
    new_blocks: u64,
    old_files: u64,
    new_files: u64,
    old_dirs: u64,
    new_dirs: u64,
    added: u64,
    removed: u64,
    resized: u64,
    mtime_only: u64,
    attrs_only: u64,
    added_bytes: u64,
    removed_bytes: u64,
    grown_bytes: u64,
    shrunk_bytes: u64,
    old_links: u64,
    new_links: u64,
}

impl Summary {
    fn file_delta(&self) -> i128 {
        self.new_files as i128 - self.old_files as i128
    }
    fn delta(&self) -> i128 {
        self.new_bytes as i128 - self.old_bytes as i128
    }
    fn block_delta(&self) -> i128 {
        self.new_blocks as i128 - self.old_blocks as i128
    }
    fn changed(&self) -> bool {
        self.added + self.removed + self.resized + self.mtime_only + self.attrs_only > 0
            || self.block_delta() != 0
            || self.old_dirs != self.new_dirs
            || self.old_links != self.new_links
    }
    fn accumulate(&mut self, s: &Self) {
        macro_rules! add { ($($f:ident),*) => { $(self.$f += s.$f;)* }; }
        add!(
            old_bytes,
            new_bytes,
            old_blocks,
            new_blocks,
            old_files,
            new_files,
            old_dirs,
            new_dirs,
            added,
            removed,
            resized,
            mtime_only,
            attrs_only,
            added_bytes,
            removed_bytes,
            grown_bytes,
            shrunk_bytes,
            old_links,
            new_links
        );
    }
}

struct FileChange {
    path: String,
    old: u64,
    new: u64,
    kind: &'static str,
}
impl FileChange {
    fn delta(&self) -> i128 {
        self.new as i128 - self.old as i128
    }
}

fn full_path(dir: &str, name: &str) -> String {
    if dir == "." {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

#[derive(Default)]
struct Rankings {
    largest: Vec<FileChange>,
    increases: Vec<FileChange>,
    decreases: Vec<FileChange>,
}

fn compare_sections(
    old: Option<&Section>,
    new: Option<&Section>,
    compare: bool,
    ranks: &mut Rankings,
    top: usize,
) -> Summary {
    let mut s = Summary::default();
    for (section, before) in [(old, true), (new, false)] {
        if let Some(section) = section {
            let bytes: u64 = section.files.iter().map(|e| e.size).sum();
            if before {
                s.old_bytes = bytes;
                s.old_blocks = section.blocks;
                s.old_files = section.files.len() as u64;
                s.old_dirs = 1;
                s.old_links = section.symlinks;
            } else {
                s.new_bytes = bytes;
                s.new_blocks = section.blocks;
                s.new_files = section.files.len() as u64;
                s.new_dirs = 1;
                s.new_links = section.symlinks;
            }
        }
    }
    let a = old.map_or(&[][..], |s| s.files.as_slice());
    let b = new.map_or(&[][..], |s| s.files.as_slice());
    let path = new.or(old).unwrap().path.as_str();
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        let order = match (a.get(i), b.get(j)) {
            (Some(x), Some(y)) => x.name.cmp(&y.name),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => unreachable!(),
        };
        let (previous, current, kind, name) = match order {
            std::cmp::Ordering::Less => {
                let x = &a[i];
                i += 1;
                s.removed += 1;
                s.removed_bytes += x.size;
                (x.size, 0, "removed", &x.name)
            }
            std::cmp::Ordering::Greater => {
                let y = &b[j];
                j += 1;
                s.added += 1;
                s.added_bytes += y.size;
                (0, y.size, "added", &y.name)
            }
            std::cmp::Ordering::Equal => {
                let (x, y) = (&a[i], &b[j]);
                i += 1;
                j += 1;
                if x.size != y.size {
                    s.resized += 1;
                    if y.size > x.size {
                        s.grown_bytes += y.size - x.size;
                    } else {
                        s.shrunk_bytes += x.size - y.size;
                    }
                } else if x.mtime != y.mtime {
                    s.mtime_only += 1;
                } else if x.attrs != y.attrs {
                    s.attrs_only += 1;
                }
                (x.size, y.size, "resized", &y.name)
            }
        };
        if current > 0 && order != std::cmp::Ordering::Less {
            let score = current as i128;
            if ranks.largest.len() < top || score > ranks.largest.last().unwrap().new as i128 {
                keep_top(
                    &mut ranks.largest,
                    FileChange {
                        path: full_path(path, name),
                        old: previous,
                        new: current,
                        kind,
                    },
                    top,
                    |c| c.new as i128,
                );
            }
        }
        if compare && current != previous {
            let c = FileChange {
                path: full_path(path, name),
                old: previous,
                new: current,
                kind,
            };
            if current > previous {
                keep_top(&mut ranks.increases, c, top, |c| c.delta());
            } else {
                keep_top(&mut ranks.decreases, c, top, |c| -c.delta());
            }
        }
    }
    s
}

fn keep_top(
    v: &mut Vec<FileChange>,
    c: FileChange,
    top: usize,
    score: impl Fn(&FileChange) -> i128,
) {
    if v.len() >= top && score(&c) <= score(v.last().unwrap()) {
        return;
    }
    let index = v.partition_point(|item| score(item) >= score(&c));
    v.insert(index, c);
    if v.len() > top {
        v.pop();
    }
}

fn bucket(path: &str, depth: usize) -> String {
    path.split('/').take(depth).collect::<Vec<_>>().join("/")
}

fn human(bytes: i128) -> String {
    let magnitude = bytes.unsigned_abs() as f64;
    let sign = if bytes < 0 { "-" } else { "" };
    for (unit, scale) in [
        ("TiB", 1_u64 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
    ] {
        if magnitude >= scale as f64 {
            return format!("{sign}{:.2} {unit}", magnitude / scale as f64);
        }
    }
    format!("{bytes} B")
}
fn signed(bytes: i128) -> String {
    if bytes > 0 {
        format!("+{}", human(bytes))
    } else {
        human(bytes)
    }
}

struct Analysis {
    directories: BTreeMap<String, Summary>,
    ranks: Rankings,
    diagnostics: Vec<Diagnostics>,
}

fn analyze(o: &Options) -> Result<Analysis> {
    let compare = o.inputs.len() == 2;
    let mut previous = HashMap::new();
    let mut diagnostics = Vec::new();
    if compare {
        let mut reader = Listing::new(
            BufReader::with_capacity(256 * 1024, File::open(&o.inputs[0])?),
            o.block_size,
            o.include_merged,
        );
        while let Some(section) = reader.next_section()? {
            let path = section.path.clone();
            if previous.insert(path.clone(), section).is_some() {
                return Err(format!("Duplicate before directory: {path}").into());
            }
        }
        if reader.root.is_none() {
            return Err("No directory headers in before input. Expected ls -lR output.".into());
        }
        diagnostics.push(reader.diagnostics);
    }
    let mut result = Analysis {
        directories: BTreeMap::new(),
        ranks: Rankings::default(),
        diagnostics,
    };
    let mut reader = Listing::new(
        BufReader::with_capacity(256 * 1024, File::open(o.inputs.last().unwrap())?),
        o.block_size,
        o.include_merged,
    );
    while let Some(section) = reader.next_section()? {
        let before = previous.remove(&section.path);
        let s = compare_sections(
            before.as_ref(),
            Some(&section),
            compare,
            &mut result.ranks,
            o.top,
        );
        if result.directories.insert(section.path.clone(), s).is_some() {
            return Err(format!("Duplicate after directory: {}", section.path).into());
        }
    }
    if reader.root.is_none() {
        return Err("No directory headers in after input. Expected ls -lR output.".into());
    }
    result.diagnostics.push(reader.diagnostics);
    let mut remaining: Vec<_> = previous.into_values().collect();
    remaining.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    for section in remaining {
        let s = compare_sections(Some(&section), None, compare, &mut result.ranks, o.top);
        result.directories.insert(section.path, s);
    }
    Ok(result)
}

fn render(w: &mut impl Write, o: &Options, a: &Analysis) -> io::Result<()> {
    let compare = o.inputs.len() == 2;
    let mut total = Summary::default();
    let mut groups = BTreeMap::<String, Summary>::new();
    for (path, s) in &a.directories {
        total.accumulate(s);
        groups
            .entry(bucket(path, o.depth))
            .or_default()
            .accumulate(s);
    }
    writeln!(w, "ls -lR directory analysis")?;
    for (i, p) in o.inputs.iter().enumerate() {
        writeln!(
            w,
            "  {}: {}",
            if compare && i == 0 { "before" } else { "after" },
            p.display()
        )?;
    }
    if compare {
        writeln!(
            w,
            "\nTotal regular-file size: {} → {} ({})",
            human(total.old_bytes as i128),
            human(total.new_bytes as i128),
            signed(total.delta())
        )?;
        writeln!(
            w,
            "Files: {} → {} | Directories: {} → {}",
            total.old_files, total.new_files, total.old_dirs, total.new_dirs
        )?;
        writeln!(
            w,
            "Added {} / removed {} / resized {} / same-size timestamp changes {} / other attribute changes {}",
            total.added, total.removed, total.resized, total.mtime_only, total.attrs_only
        )?;
        writeln!(
            w,
            "Size change: new files {} + existing growth {} - removed {} - existing shrinkage {}",
            human(total.added_bytes as i128),
            human(total.grown_bytes as i128),
            human(total.removed_bytes as i128),
            human(total.shrunk_bytes as i128)
        )?;
        writeln!(
            w,
            "ls block total: {} → {} ({})",
            human(total.old_blocks as i128),
            human(total.new_blocks as i128),
            signed(total.block_delta())
        )?;
        if let Some(days) = o.days {
            writeln!(
                w,
                "Average net growth over {days} days: {}/day (user-supplied interval, not a forecast)",
                signed((total.delta() as f64 / days).round() as i128)
            )?;
        }
    } else {
        writeln!(
            w,
            "\nRegular-file size {} | Files {} | Directories {} | Symlinks {}",
            human(total.new_bytes as i128),
            total.new_files,
            total.new_dirs,
            total.new_links
        )?;
        writeln!(w, "ls block total: {}", human(total.new_blocks as i128))?;
    }
    writeln!(
        w,
        "\nPath summary (depth {}, descendants included; each file counted once)",
        o.depth
    )?;
    writeln!(
        w,
        "  {:>12} {:>12} {:>12}  {:>10}  Path",
        "Current size", "Change", "Block change", "Files"
    )?;
    let mut ordered: Vec<_> = groups.iter().collect();
    ordered.sort_by(|(pa, a), (pb, b)| {
        let key = |s: &Summary| {
            if compare {
                s.delta().unsigned_abs()
            } else {
                s.new_bytes as u128
            }
        };
        key(b).cmp(&key(a)).then_with(|| pa.cmp(pb))
    });
    for (p, s) in ordered.into_iter().take(o.top) {
        writeln!(
            w,
            "  {:>12} {:>12} {:>12}  {:>10}  {}",
            human(s.new_bytes as i128),
            if compare {
                signed(s.delta())
            } else {
                "—".into()
            },
            if compare {
                signed(s.block_delta())
            } else {
                "—".into()
            },
            s.new_files,
            p
        )?;
    }
    if groups.len() > o.top {
        writeln!(
            w,
            "  {} more paths omitted (adjust --top)",
            groups.len() - o.top
        )?;
    }
    if !compare {
        let mut dirs: Vec<_> = a
            .directories
            .iter()
            .filter(|(_, s)| s.new_files > 0)
            .collect();
        dirs.sort_by(|(pa, a), (pb, b)| b.new_bytes.cmp(&a.new_bytes).then_with(|| pa.cmp(pb)));
        writeln!(
            w,
            "\nLargest directories (direct files only; descendants excluded)"
        )?;
        for (p, s) in dirs.into_iter().take(o.top) {
            writeln!(
                w,
                "  {:>12}  files {:>8}  {}",
                human(s.new_bytes as i128),
                s.new_files,
                p
            )?;
        }
    }
    if compare {
        writeln!(
            w,
            "\nDirectory file-count changes (direct regular files; rough inode-count indicator)"
        )?;
        let mut dirs: Vec<_> = a
            .directories
            .iter()
            .filter(|(_, s)| s.file_delta() != 0)
            .collect();
        dirs.sort_by(|(pa, a), (pb, b)| {
            b.file_delta()
                .unsigned_abs()
                .cmp(&a.file_delta().unsigned_abs())
                .then_with(|| pa.cmp(pb))
        });
        if dirs.is_empty() {
            writeln!(w, "  None")?;
        }
        for (p, s) in dirs.into_iter().take(o.top) {
            writeln!(
                w,
                "  {:+10} files  {} → {} | size {}  {}",
                s.file_delta(),
                s.old_files,
                s.new_files,
                signed(s.delta()),
                p
            )?;
        }
        for (title, positive) in [
            ("Growing directories", true),
            ("Shrinking directories", false),
        ] {
            let mut dirs: Vec<_> = a
                .directories
                .iter()
                .filter(|(_, s)| {
                    if positive {
                        s.delta() > 0
                    } else {
                        s.delta() < 0
                    }
                })
                .collect();
            dirs.sort_by(|(pa, a), (pb, b)| {
                if positive {
                    b.delta().cmp(&a.delta()).then_with(|| pa.cmp(pb))
                } else {
                    a.delta().cmp(&b.delta()).then_with(|| pa.cmp(pb))
                }
            });
            writeln!(w, "\n{title} (direct files only; descendants excluded)")?;
            if dirs.is_empty() {
                writeln!(w, "  None")?;
            }
            for (p, s) in dirs.into_iter().take(o.top) {
                writeln!(
                    w,
                    "  {:>12}  added {:>6} / removed {:>6} / resized {:>5}  {}",
                    signed(s.delta()),
                    s.added,
                    s.removed,
                    s.resized,
                    p
                )?;
                writeln!(
                    w,
                    "                new files {} / existing growth {} / removed {} / existing shrinkage {}",
                    human(s.added_bytes as i128),
                    human(s.grown_bytes as i128),
                    human(s.removed_bytes as i128),
                    human(s.shrunk_bytes as i128)
                )?;
            }
        }
        for (title, files) in [
            ("Largest file increases", &a.ranks.increases),
            ("Largest file decreases and removals", &a.ranks.decreases),
        ] {
            writeln!(w, "\n{title}")?;
            if files.is_empty() {
                writeln!(w, "  None")?;
            }
            for c in files {
                writeln!(
                    w,
                    "  {:>12}  {} → {} [{}] {}",
                    signed(c.delta()),
                    human(c.old as i128),
                    human(c.new as i128),
                    c.kind,
                    c.path
                )?;
            }
        }
        let no_growth = a
            .directories
            .values()
            .filter(|s| s.changed() && s.delta() == 0)
            .count();
        writeln!(
            w,
            "\nChanged directories with no net size growth: {no_growth} (includes rotation, replacement, timestamp, attribute, and block changes)"
        )?;
        let mut block_only: Vec<_> = a
            .directories
            .iter()
            .filter(|(_, s)| s.delta() == 0 && s.block_delta() != 0)
            .collect();
        block_only.sort_by(|(pa, a), (pb, b)| {
            b.block_delta()
                .unsigned_abs()
                .cmp(&a.block_delta().unsigned_abs())
                .then_with(|| pa.cmp(pb))
        });
        if !block_only.is_empty() {
            writeln!(
                w,
                "\nDirectories with block changes but no net file-size change (direct totals)"
            )?;
            for (p, s) in block_only.into_iter().take(o.top) {
                writeln!(w, "  {:>12}  {}", signed(s.block_delta()), p)?;
            }
        }
    }
    writeln!(w, "\nLargest current files (capacity, not recent growth)")?;
    for c in &a.ranks.largest {
        writeln!(w, "  {:>12}  {}", human(c.new as i128), c.path)?;
    }
    writeln!(w, "\nNotes")?;
    writeln!(
        w,
        "  File sizes are logical regular-file sizes. ls blocks are totals × {} bytes.",
        o.block_size
    )?;
    writeln!(
        w,
        "  Block totals include directories, links, and other entries. Hard-link sharing, shared blocks, and deleted open files cannot be identified."
    )?;
    writeln!(
        w,
        "  Content changes with unchanged size and displayed timestamp are undetectable. Both listings must cover the same complete logical root."
    )?;
    if o.include_merged {
        writeln!(
            w,
            "  merged included: unified views can duplicate storage layers and overstate disk usage."
        )?;
    }
    for (i, d) in a.diagnostics.iter().enumerate() {
        let label = if compare && i == 0 { "before" } else { "after" };
        if d.excluded_dirs > 0 {
            writeln!(
                w,
                "  {label}: overlay merged excluded: {} directories / {} files / {}",
                d.excluded_dirs,
                d.excluded_files,
                human(d.excluded_bytes as i128)
            )?;
        }
        if d.malformed + d.errors + d.missing_total > 0 {
            writeln!(
                w,
                "  Warning {label}: {} unparsed lines / {} ls errors / {} directories without totals; results may be incomplete",
                d.malformed, d.errors, d.missing_total
            )?;
            for e in &d.examples {
                writeln!(w, "    {e}")?;
            }
        }
    }
    Ok(())
}

fn run() -> Result<()> {
    if let Some(o) = options()? {
        let a = analyze(&o)?;
        let stdout = io::stdout();
        let mut out = io::BufWriter::new(stdout.lock());
        render(&mut out, &o, &a)?;
        out.flush()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(text: &str) -> Vec<Section> {
        let mut l = Listing::new(io::Cursor::new(text), 1024, false);
        let mut v = Vec::new();
        while let Some(s) = l.next_section().unwrap() {
            v.push(s);
        }
        v
    }
    #[test]
    fn localized_dates_spaces_and_colon_filenames() {
        let s = parse(
            ".:\ntotal 4\n-rw-r--r-- 1 root root 123 Oct  1 17:52 file with spaces:\n./child:\ntotal 8\n-rw-r--r-- 1 user group 45 Sep 28 2025 older file.txt\n",
        );
        assert_eq!(s.len(), 2);
        assert_eq!(&*s[0].files[0].name, "file with spaces:");
        assert_eq!(s[0].blocks, 4096);
        assert_eq!(s[1].path, "child");
        assert_eq!(s[1].files[0].size, 45);
    }
    #[test]
    fn growth_deletion_rotation_and_mtime_have_distinct_accounting() {
        let a = parse(
            ".:\ntotal 8\n-rw-r--r-- 1 u g 100 Sep 28 12:00 grow\n-rw-r--r-- 1 u g 50 Sep 28 12:00 old.log\n-rw-r--r-- 1 u g 32 Sep 28 12:00 perf\n-rw-r--r-- 1 u g 80 Sep 28 12:00 shrink\n",
        );
        let b = parse(
            ".:\ntotal 8\n-rw-r--r-- 1 u g 130 Oct 1 12:00 grow\n-rw-r--r-- 1 u g 50 Oct 1 12:00 new.log\n-rw-r--r-- 1 u g 32 Oct 1 12:00 perf\n-rw-r--r-- 1 u g 60 Oct 1 12:00 shrink\n",
        );
        let s = compare_sections(Some(&a[0]), Some(&b[0]), true, &mut Rankings::default(), 10);
        assert_eq!(s.delta(), 10);
        assert_eq!((s.added, s.removed, s.resized, s.mtime_only), (1, 1, 2, 1));
        assert_eq!(
            (
                s.added_bytes,
                s.removed_bytes,
                s.grown_bytes,
                s.shrunk_bytes
            ),
            (50, 50, 30, 20)
        );
        assert_eq!(
            s.delta(),
            s.added_bytes as i128 + s.grown_bytes as i128
                - s.removed_bytes as i128
                - s.shrunk_bytes as i128
        );
    }
    #[test]
    fn excludes_only_overlay_merged_and_keeps_empty_directories() {
        let s = parse(
            ".:\ntotal 0\n./containers/storage/overlay/abc/merged:\ntotal 4\n-rw-r--r-- 1 u g 99 Oct 1 12:00 duplicate\n./merged:\ntotal 4\n-rw-r--r-- 1 u g 10 Oct 1 12:00 real\n./empty:\ntotal 0\n",
        );
        assert_eq!(
            s.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            [".", "merged", "empty"]
        );
        assert_eq!(
            s.iter().flat_map(|s| &s.files).map(|e| e.size).sum::<u64>(),
            10
        );
    }
    #[test]
    fn roots_normalize_and_symlinks_do_not_count_as_regular_files() {
        let s = parse(
            "/data:\ntotal 4\nlrwxrwxrwx 1 u g 8 Sep 28 12:00 link -> target\n/data/a:\ntotal 0\n",
        );
        assert_eq!(s[0].path, ".");
        assert_eq!(s[0].symlinks, 1);
        assert!(s[0].files.is_empty());
        assert_eq!(s[1].path, "a");
        assert_eq!(relative("data", "data/a/b").unwrap(), "a/b");
        assert_eq!(relative("/", "/a").unwrap(), "a");
        let relative_root = parse("data:\ntotal 0\ndata/long_directory_name:\ntotal 0\n");
        assert_eq!(relative_root.len(), 2);
        assert_eq!(relative_root[1].path, "long_directory_name");
    }
    #[test]
    fn block_only_changes_remain_visible_and_ranking_is_bounded() {
        let a = parse(".:\ntotal 4\n-rw-r--r-- 1 u g 100 Sep 28 12:00 same\n");
        let b = parse(".:\ntotal 8\n-rw-r--r-- 1 u g 100 Sep 28 12:00 same\n");
        let s = compare_sections(Some(&a[0]), Some(&b[0]), true, &mut Rankings::default(), 1);
        assert_eq!(s.delta(), 0);
        assert_eq!(s.block_delta(), 4096);
        assert!(s.changed());
        let mut up = Vec::new();
        keep_top(
            &mut up,
            FileChange {
                path: "a".into(),
                old: 0,
                new: 3,
                kind: "added",
            },
            1,
            |c| c.delta(),
        );
        keep_top(
            &mut up,
            FileChange {
                path: "b".into(),
                old: 0,
                new: 8,
                kind: "added",
            },
            1,
            |c| c.delta(),
        );
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].path, "b");
    }
}
