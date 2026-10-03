//! Capacity analysis, snapshot comparison, and human-readable reports.
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};

use crate::{Diagnostics, Options, Result, Section, input};

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
        self.added + self.removed + self.resized + self.mtime_only + self.attrs_only > 0
            || self.block_delta() != 0
            || self.old_dirs != self.new_dirs
            || self.old_links != self.new_links
    }
    const fn accumulate(&mut self, s: &Self) {
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

fn compare_sections(
    old: Option<&Section>,
    new: Option<&Section>,
    compare: bool,
    ranks: &mut Rankings,
    top: usize,
) -> Summary {
    let mut summary = Summary::default();
    for (section, before) in [(old, true), (new, false)] {
        if let Some(section) = section {
            let bytes: u64 = section.files.iter().map(|e| e.size).sum();
            if before {
                summary.old_bytes = bytes;
                summary.old_blocks = section.blocks;
                summary.old_files = section.files.len() as u64;
                summary.old_dirs = 1;
                summary.old_links = section.symlinks;
            } else {
                summary.new_bytes = bytes;
                summary.new_blocks = section.blocks;
                summary.new_files = section.files.len() as u64;
                summary.new_dirs = 1;
                summary.new_links = section.symlinks;
            }
        }
    }
    let old_files = old.map_or(&[][..], |summary| summary.files.as_slice());
    let new_files = new.map_or(&[][..], |summary| summary.files.as_slice());
    let path = new.or(old).unwrap().path.as_str();
    let (mut i, mut j) = (0, 0);
    while i < old_files.len() || j < new_files.len() {
        let order = match (old_files.get(i), new_files.get(j)) {
            (Some(x), Some(y)) => x.name.cmp(&y.name),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => unreachable!(),
        };
        let (previous, current, kind, name) = match order {
            std::cmp::Ordering::Less => {
                let x = &old_files[i];
                i += 1;
                summary.removed += 1;
                summary.removed_bytes += x.size;
                (x.size, 0, "removed", &x.name)
            }
            std::cmp::Ordering::Greater => {
                let y = &new_files[j];
                j += 1;
                summary.added += 1;
                summary.added_bytes += y.size;
                (0, y.size, "added", &y.name)
            }
            std::cmp::Ordering::Equal => {
                let (x, y) = (&old_files[i], &new_files[j]);
                i += 1;
                j += 1;
                if x.size != y.size {
                    summary.resized += 1;
                    if y.size > x.size {
                        summary.grown_bytes += y.size - x.size;
                    } else {
                        summary.shrunk_bytes += x.size - y.size;
                    }
                } else if x.mtime != y.mtime {
                    summary.mtime_only += 1;
                } else if x.attrs != y.attrs {
                    summary.attrs_only += 1;
                }
                (x.size, y.size, "resized", &y.name)
            }
        };
        if current > 0 && order != std::cmp::Ordering::Less {
            let score = i128::from(current);
            if ranks.largest.len() < top || score > i128::from(ranks.largest.last().unwrap().new) {
                keep_top(
                    &mut ranks.largest,
                    FileChange {
                        path: full_path(path, name),
                        old: previous,
                        new: current,
                        kind,
                    },
                    top,
                    |c| i128::from(c.new),
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
                keep_top(&mut ranks.increases, c, top, FileChange::delta);
            } else {
                keep_top(&mut ranks.decreases, c, top, |c| -c.delta());
            }
        }
    }
    summary
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
    directories: BTreeMap<String, Summary>,
    ranks: Rankings,
    diagnostics: Vec<Diagnostics>,
}

pub fn analyze(o: &Options) -> Result<Analysis> {
    let compare = o.inputs.len() == 2;
    let mut previous = HashMap::new();
    let mut diagnostics = Vec::new();
    if compare {
        let mut reader = input::Input::open(&o.inputs[0], o)?;
        while let Some(section) = reader.next_section()? {
            let path = section.path.clone();
            if previous.insert(path.clone(), section).is_some() {
                return Err(format!("Duplicate before directory: {path}").into());
            }
        }
        if !reader.has_root() {
            return Err("No directory headers in before input. Expected ls -lR output.".into());
        }
        diagnostics.push(reader.into_diagnostics());
    }
    let mut result = Analysis {
        directories: BTreeMap::new(),
        ranks: Rankings::default(),
        diagnostics,
    };
    let mut reader = input::Input::open(o.inputs.last().unwrap(), o)?;
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
    if !reader.has_root() {
        return Err("No directory headers in after input. Expected ls -lR output.".into());
    }
    result.diagnostics.push(reader.into_diagnostics());
    let mut remaining: Vec<_> = previous.into_values().collect();
    remaining.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    for section in remaining {
        let s = compare_sections(Some(&section), None, compare, &mut result.ranks, o.top);
        result.directories.insert(section.path, s);
    }
    Ok(result)
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
    let mut ordered: Vec<_> = groups.iter().collect();
    ordered.sort_by(|(pa, a), (pb, b)| {
        let key = |s: &Summary| {
            if compare {
                s.delta().unsigned_abs()
            } else {
                u128::from(s.new_bytes)
            }
        };
        key(b).cmp(&key(a)).then_with(|| pa.cmp(pb))
    });
    for (p, s) in ordered.into_iter().take(o.top) {
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
    use crate::{Listing, relative};

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
            i128::from(s.added_bytes) + i128::from(s.grown_bytes)
                - i128::from(s.removed_bytes)
                - i128::from(s.shrunk_bytes)
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
            super::FileChange::delta,
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
            super::FileChange::delta,
        );
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].path, "b");
    }
}
