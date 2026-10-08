//! Capacity analysis, snapshot comparison, and human-readable reports.
use std::io::{self, Write};

use crate::sort::{Record, Sorter};
use crate::top::{Tie, Top};
use crate::{Diagnostics, Options, Result, input};

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
        i128::from(self.new_files) - i128::from(self.old_files)
    }
    fn delta(&self) -> i128 {
        i128::from(self.new_bytes) - i128::from(self.old_bytes)
    }
    fn block_delta(&self) -> i128 {
        i128::from(self.new_blocks) - i128::from(self.old_blocks)
    }
    fn changed(&self) -> bool {
        self.added != 0
            || self.removed != 0
            || self.resized != 0
            || self.mtime_only != 0
            || self.attrs_only != 0
            || self.block_delta() != 0
            || self.old_dirs != self.new_dirs
            || self.old_links != self.new_links
    }
    fn accumulate(&mut self, s: &Self) -> Result<()> {
        macro_rules! add { ($($f:ident),*) => { $(self.$f = self.$f.checked_add(s.$f).ok_or(concat!("Summary overflow: ", stringify!($f)))?;)* }; }
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
        Ok(())
    }
    fn encode(&self) -> Vec<u8> {
        input::encode(&[
            self.old_bytes,
            self.new_bytes,
            self.old_blocks,
            self.new_blocks,
            self.old_files,
            self.new_files,
            self.old_dirs,
            self.new_dirs,
            self.added,
            self.removed,
            self.resized,
            self.mtime_only,
            self.attrs_only,
            self.added_bytes,
            self.removed_bytes,
            self.grown_bytes,
            self.shrunk_bytes,
            self.old_links,
            self.new_links,
        ])
    }
    fn decode(data: &[u8]) -> Result<Self> {
        let n = input::numbers::<19>(data)?;
        Ok(Self {
            old_bytes: n[0],
            new_bytes: n[1],
            old_blocks: n[2],
            new_blocks: n[3],
            old_files: n[4],
            new_files: n[5],
            old_dirs: n[6],
            new_dirs: n[7],
            added: n[8],
            removed: n[9],
            resized: n[10],
            mtime_only: n[11],
            attrs_only: n[12],
            added_bytes: n[13],
            removed_bytes: n[14],
            grown_bytes: n[15],
            shrunk_bytes: n[16],
            old_links: n[17],
            new_links: n[18],
        })
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
        i128::from(self.new) - i128::from(self.old)
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

fn bucket(path: &str, depth: usize) -> String {
    path.split('/').take(depth).collect::<Vec<_>>().join("/")
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Human-readable sizes intentionally round to two decimal places"
)]
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

pub struct Analysis {
    total: Summary,
    groups: Vec<(String, Summary)>,
    group_count: u64,
    largest_dirs: Vec<(String, Summary)>,
    count_dirs: Vec<(String, Summary)>,
    growing_dirs: Vec<(String, Summary)>,
    shrinking_dirs: Vec<(String, Summary)>,
    block_dirs: Vec<(String, Summary)>,
    no_growth: u64,
    ranks: Rankings,
    diagnostics: Vec<Diagnostics>,
}

struct Accumulator {
    total: Summary,
    groups: Sorter,
    largest_dirs: Top<(String, Summary)>,
    count_dirs: Top<(String, Summary)>,
    growing_dirs: Top<(String, Summary)>,
    shrinking_dirs: Top<(String, Summary)>,
    block_dirs: Top<(String, Summary)>,
    largest: Top<FileChange>,
    increases: Top<FileChange>,
    decreases: Top<FileChange>,
    no_growth: u64,
}

fn path_tie(path: &str) -> Tie {
    (0, 0, path.into(), String::new())
}

impl Accumulator {
    fn directory(&mut self, path: String, s: Summary, depth: usize) -> Result<()> {
        self.total.accumulate(&s)?;
        self.groups.push(Record {
            key: (bucket(&path, depth), path.clone()),
            data: s.encode(),
        })?;
        let offer = |top: &mut Top<(String, Summary)>, score: i128| {
            top.offer(score, path_tie(&path), (path.clone(), s.clone()));
        };
        if s.new_files > 0 {
            offer(&mut self.largest_dirs, i128::from(s.new_bytes));
        }
        if s.file_delta() != 0 {
            offer(&mut self.count_dirs, s.file_delta().abs());
        }
        if s.delta() > 0 {
            offer(&mut self.growing_dirs, s.delta());
        }
        if s.delta() < 0 {
            offer(&mut self.shrinking_dirs, -s.delta());
        }
        if s.changed() && s.delta() == 0 {
            self.no_growth += 1;
        }
        if s.delta() == 0 && s.block_delta() != 0 {
            offer(&mut self.block_dirs, s.block_delta().abs());
        }
        Ok(())
    }

    fn file(
        &mut self,
        old: Option<&Record>,
        new: Option<&Record>,
        s: &mut Summary,
        compare: bool,
        new_order: Option<u64>,
    ) -> Result<()> {
        let x = old.map(|r| input::numbers::<3>(&r.data)).transpose()?;
        let y = new.map(|r| input::numbers::<3>(&r.data)).transpose()?;
        let previous = x.map_or(0, |n| n[0]);
        let current = y.map_or(0, |n| n[0]);
        if x.is_some() {
            s.old_files += 1;
            s.old_bytes = s
                .old_bytes
                .checked_add(previous)
                .ok_or("Directory size overflow")?;
        }
        if y.is_some() {
            s.new_files += 1;
            s.new_bytes = s
                .new_bytes
                .checked_add(current)
                .ok_or("Directory size overflow")?;
        }
        let kind = match (x, y) {
            (Some(_), None) => {
                s.removed += 1;
                s.removed_bytes = s
                    .removed_bytes
                    .checked_add(previous)
                    .ok_or("Removed size overflow")?;
                "removed"
            }
            (None, Some(_)) => {
                s.added += 1;
                s.added_bytes = s
                    .added_bytes
                    .checked_add(current)
                    .ok_or("Added size overflow")?;
                "added"
            }
            (Some(x), Some(y)) => {
                if previous != current {
                    s.resized += 1;
                    if current > previous {
                        s.grown_bytes = s
                            .grown_bytes
                            .checked_add(current - previous)
                            .ok_or("Growth overflow")?;
                    } else {
                        s.shrunk_bytes = s
                            .shrunk_bytes
                            .checked_add(previous - current)
                            .ok_or("Shrinkage overflow")?;
                    }
                } else if x[1] != y[1] {
                    s.mtime_only += 1;
                } else if x[2] != y[2] {
                    s.attrs_only += 1;
                }
                "resized"
            }
            _ => unreachable!(),
        };
        let record = new.or(old).unwrap();
        let name = &record.key.1[1..];
        // Old behavior visits after directories in source order, with names
        // sorted, then wholly removed directories in lexical path order.
        let tie = (
            u8::from(new_order.is_none()),
            new_order.unwrap_or(0),
            record.key.0.clone(),
            name.into(),
        );
        let change = || FileChange {
            path: full_path(&record.key.0, name),
            old: previous,
            new: current,
            kind,
        };
        if y.is_some() && current > 0 {
            self.largest
                .offer(i128::from(current), tie.clone(), change());
        }
        if compare && current != previous {
            if current > previous {
                self.increases
                    .offer(i128::from(current) - i128::from(previous), tie, change());
            } else {
                self.decreases
                    .offer(i128::from(previous) - i128::from(current), tie, change());
            }
        }
        Ok(())
    }
}

pub fn analyze(o: &Options) -> Result<Analysis> {
    let compare = o.inputs.len() == 2;
    let mut before = if compare {
        Some(input::snapshot(&o.inputs[0], o)?)
    } else {
        None
    };
    let mut after = input::snapshot(o.inputs.last().unwrap(), o)?;
    let mut a = Accumulator {
        total: Summary::default(),
        groups: Sorter::new()?,
        no_growth: 0,
        largest_dirs: Top::new(o.top),
        count_dirs: Top::new(o.top),
        growing_dirs: Top::new(o.top),
        shrinking_dirs: Top::new(o.top),
        block_dirs: Top::new(o.top),
        largest: Top::new(o.top),
        increases: Top::new(o.top),
        decreases: Top::new(o.top),
    };
    let mut x = before
        .as_mut()
        .map(input::Snapshot::next)
        .transpose()?
        .flatten();
    let mut y = after.next()?;
    let mut path = None;
    let mut summary = Summary::default();
    let mut new_order = None;
    while x.is_some() || y.is_some() {
        let order = match (&x, &y) {
            (Some(x), Some(y)) => x.key.cmp(&y.key),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => unreachable!(),
        };
        let old = if order.is_le() { x.as_ref() } else { None };
        let new = if order.is_ge() { y.as_ref() } else { None };
        let record = new.or(old).unwrap();
        if path.as_ref() != Some(&record.key.0) {
            if let Some(path) = path.take() {
                a.directory(path, summary, o.depth)?;
            }
            path = Some(record.key.0.clone());
            summary = Summary::default();
            new_order = None;
        }
        if record.key.1 == "0" {
            if let Some(old) = old {
                let n = input::numbers::<3>(&old.data)?;
                summary.old_dirs = 1;
                summary.old_blocks = n[0];
                summary.old_links = n[1];
            }
            if let Some(new) = new {
                let n = input::numbers::<3>(&new.data)?;
                summary.new_dirs = 1;
                summary.new_blocks = n[0];
                summary.new_links = n[1];
                new_order = Some(n[2]);
            }
        } else {
            a.file(old, new, &mut summary, compare, new_order)?;
        }
        if order.is_le() {
            x = before.as_mut().unwrap().next()?;
        }
        if order.is_ge() {
            y = after.next()?;
        }
    }
    if let Some(path) = path {
        a.directory(path, summary, o.depth)?;
    }
    let mut diagnostics = Vec::new();
    if let Some(before) = before {
        diagnostics.push(before.diagnostics);
    }
    diagnostics.push(after.diagnostics);
    let mut groups = a.groups.finish()?;
    let mut group_top = Top::new(o.top);
    let mut group_count = 0;
    let mut group = None;
    let mut summary = Summary::default();
    while let Some(record) = groups.next()? {
        if group.as_ref() != Some(&record.key.0) {
            if let Some(path) = group.take() {
                let score = if compare {
                    summary.delta().abs()
                } else {
                    i128::from(summary.new_bytes)
                };
                group_top.offer(score, path_tie(&path), (path, summary));
            }
            group = Some(record.key.0);
            group_count += 1;
            summary = Summary::default();
        }
        summary.accumulate(&Summary::decode(&record.data)?)?;
    }
    if let Some(path) = group {
        let score = if compare {
            summary.delta().abs()
        } else {
            i128::from(summary.new_bytes)
        };
        group_top.offer(score, path_tie(&path), (path, summary));
    }
    Ok(Analysis {
        total: a.total,
        groups: group_top.finish(),
        group_count,
        largest_dirs: a.largest_dirs.finish(),
        count_dirs: a.count_dirs.finish(),
        growing_dirs: a.growing_dirs.finish(),
        shrinking_dirs: a.shrinking_dirs.finish(),
        block_dirs: a.block_dirs.finish(),
        no_growth: a.no_growth,
        ranks: Rankings {
            largest: a.largest.finish(),
            increases: a.increases.finish(),
            decreases: a.decreases.finish(),
        },
        diagnostics,
    })
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "The displayed daily average is intentionally rounded to whole bytes"
)]
fn daily_growth(bytes: i128, days: f64) -> String {
    signed((bytes as f64 / days).round() as i128)
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep the existing report layout in display order"
)]
pub fn render(w: &mut impl Write, o: &Options, a: &Analysis) -> io::Result<()> {
    let compare = o.inputs.len() == 2;
    let total = &a.total;
    let groups = &a.groups;
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
            human(i128::from(total.old_bytes)),
            human(i128::from(total.new_bytes)),
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
            human(i128::from(total.added_bytes)),
            human(i128::from(total.grown_bytes)),
            human(i128::from(total.removed_bytes)),
            human(i128::from(total.shrunk_bytes))
        )?;
        writeln!(
            w,
            "ls block total: {} → {} ({})",
            human(i128::from(total.old_blocks)),
            human(i128::from(total.new_blocks)),
            signed(total.block_delta())
        )?;
        if let Some(days) = o.days {
            writeln!(
                w,
                "Average net growth over {days} days: {}/day (user-supplied interval, not a forecast)",
                daily_growth(total.delta(), days)
            )?;
        }
    } else {
        writeln!(
            w,
            "\nRegular-file size {} | Files {} | Directories {} | Symlinks {}",
            human(i128::from(total.new_bytes)),
            total.new_files,
            total.new_dirs,
            total.new_links
        )?;
        writeln!(w, "ls block total: {}", human(i128::from(total.new_blocks)))?;
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
    for (p, s) in groups {
        writeln!(
            w,
            "  {:>12} {:>12} {:>12}  {:>10}  {}",
            human(i128::from(s.new_bytes)),
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
    if a.group_count > o.top as u64 {
        writeln!(
            w,
            "  {} more paths omitted (adjust --top)",
            a.group_count - o.top as u64
        )?;
    }
    if !compare {
        let dirs = &a.largest_dirs;
        writeln!(
            w,
            "\nLargest directories (direct files only; descendants excluded)"
        )?;
        for (p, s) in dirs {
            writeln!(
                w,
                "  {:>12}  files {:>8}  {}",
                human(i128::from(s.new_bytes)),
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
        let dirs = &a.count_dirs;
        if dirs.is_empty() {
            writeln!(w, "  None")?;
        }
        for (p, s) in dirs {
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
            let dirs = if positive {
                &a.growing_dirs
            } else {
                &a.shrinking_dirs
            };
            writeln!(w, "\n{title} (direct files only; descendants excluded)")?;
            if dirs.is_empty() {
                writeln!(w, "  None")?;
            }
            for (p, s) in dirs {
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
                    human(i128::from(s.added_bytes)),
                    human(i128::from(s.grown_bytes)),
                    human(i128::from(s.removed_bytes)),
                    human(i128::from(s.shrunk_bytes))
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
                    human(i128::from(c.old)),
                    human(i128::from(c.new)),
                    c.kind,
                    c.path
                )?;
            }
        }
        let no_growth = a.no_growth;
        writeln!(
            w,
            "\nChanged directories with no net size growth: {no_growth} (includes rotation, replacement, timestamp, attribute, and block changes)"
        )?;
        let block_only = &a.block_dirs;
        if !block_only.is_empty() {
            writeln!(
                w,
                "\nDirectories with block changes but no net file-size change (direct totals)"
            )?;
            for (p, s) in block_only {
                writeln!(w, "  {:>12}  {}", signed(s.block_delta()), p)?;
            }
        }
    }
    writeln!(w, "\nLargest current files (capacity, not recent growth)")?;
    for c in &a.ranks.largest {
        writeln!(w, "  {:>12}  {}", human(i128::from(c.new)), c.path)?;
    }
    writeln!(w, "\nNotes")?;
    writeln!(
        w,
        "  File sizes are logical regular-file sizes. ls total × {} bytes; find total_bytes is already bytes.",
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
                human(i128::from(d.excluded_bytes))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Mode;
    use crate::export::temp::Staging;

    fn analyze_text(old: Option<&str>, new: &str) -> Analysis {
        let stage = Staging::new(&std::env::temp_dir()).unwrap();
        let after = stage.0.join("after");
        std::fs::write(&after, new).unwrap();
        let mut inputs = Vec::new();
        if let Some(old) = old {
            let before = stage.0.join("before");
            std::fs::write(&before, old).unwrap();
            inputs.push(before);
        }
        inputs.push(after);
        analyze(&Options {
            inputs,
            depth: 1,
            top: 10,
            block_size: 1024,
            include_merged: false,
            days: None,
            format: "text".into(),
            input_format: "ls".into(),
            output: None,
            sqlite3_bin: "sqlite3".into(),
            duckdb_bin: "duckdb".into(),
            mode: Mode::Report,
            source: "ls".into(),
        })
        .unwrap()
    }

    #[test]
    fn localized_dates_spaces_and_colon_filenames() {
        let a = analyze_text(
            None,
            ".:\ntotal 4\n-rw-r--r-- 1 root root 123 Oct  1 17:52 file with spaces:\n./child:\n합계 8\n-rw-r--r-- 1 user group 45 Sep 28 2025 older file.txt\n",
        );
        assert_eq!(a.total.new_dirs, 2);
        assert_eq!(a.total.new_blocks, 12288);
        assert_eq!(a.ranks.largest[0].path, "file with spaces:");
        assert_eq!(a.ranks.largest[1].path, "child/older file.txt");
    }

    #[test]
    fn growth_deletion_rotation_and_mtime_have_distinct_accounting() {
        let a = analyze_text(
            Some(
                ".:\ntotal 8\n-rw-r--r-- 1 u g 100 Sep 28 12:00 grow\n-rw-r--r-- 1 u g 50 Sep 28 12:00 old.log\n-rw-r--r-- 1 u g 32 Sep 28 12:00 perf\n-rw-r--r-- 1 u g 80 Sep 28 12:00 shrink\n",
            ),
            ".:\ntotal 8\n-rw-r--r-- 1 u g 130 Oct 1 12:00 grow\n-rw-r--r-- 1 u g 50 Oct 1 12:00 new.log\n-rw-r--r-- 1 u g 32 Oct 1 12:00 perf\n-rw-r--r-- 1 u g 60 Oct 1 12:00 shrink\n",
        );
        let s = a.total;
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
            i128::from(s.added_bytes) + i128::from(s.grown_bytes)
                - i128::from(s.removed_bytes)
                - i128::from(s.shrunk_bytes)
        );
    }

    #[test]
    fn excludes_only_overlay_merged_and_keeps_empty_directories() {
        let a = analyze_text(
            None,
            ".:\ntotal 0\n./containers/storage/overlay/abc/merged:\ntotal 4\n-rw-r--r-- 1 u g 99 Oct 1 12:00 duplicate\n./merged:\ntotal 4\n-rw-r--r-- 1 u g 10 Oct 1 12:00 real\n./empty:\ntotal 0\n",
        );
        assert_eq!(a.total.new_dirs, 3);
        assert_eq!(a.total.new_bytes, 10);
        assert_eq!(a.diagnostics[0].excluded_dirs, 1);
        assert_eq!(a.diagnostics[0].excluded_files, 1);
    }

    #[test]
    fn roots_normalize_and_symlinks_do_not_count_as_regular_files() {
        let a = analyze_text(
            None,
            "/data:\ntotal 4\nlrwxrwxrwx 1 u g 8 Sep 28 12:00 link -> target\n/data/a:\ntotal 0\n",
        );
        assert_eq!(a.total.new_links, 1);
        assert_eq!(a.total.new_files, 0);
        assert_eq!(a.total.new_dirs, 2);
        assert_eq!(crate::relative("data", "data/a/b").unwrap(), "a/b");
        assert_eq!(crate::relative("/", "/a").unwrap(), "a");
        let a = analyze_text(None, "data:\ntotal 0\ndata/long_directory_name:\ntotal 0\n");
        assert_eq!(a.total.new_dirs, 2);
    }

    #[test]
    fn block_only_changes_remain_visible() {
        let a = analyze_text(
            Some(".:\ntotal 4\n-rw-r--r-- 1 u g 100 Sep 28 12:00 same\n"),
            ".:\ntotal 8\n-rw-r--r-- 1 u g 100 Sep 28 12:00 same\n",
        );
        assert_eq!(a.total.delta(), 0);
        assert_eq!(a.total.block_delta(), 4096);
        assert_eq!(a.no_growth, 1);
        assert_eq!(a.block_dirs.len(), 1);
    }
}
