//! Exercise disk runs and the real report pipeline, rather than a test-only parser.
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "stream-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("tmp")).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str]) -> Output {
        let result = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(args)
            .current_dir(&self.0)
            .env("TMPDIR", self.0.join("tmp"))
            .output()
            .unwrap();
        assert_eq!(
            fs::read_dir(self.0.join("tmp")).unwrap().count(),
            0,
            "Temporary artifacts leaked"
        );
        result
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn large_reordered_comparison_merges_runs_and_preserves_tie_order() {
    let tree = Tree::new();
    for after in [false, true] {
        let path = tree.0.join(if after { "after" } else { "before" });
        let mut w = BufWriter::new(File::create(path).unwrap());
        writeln!(w, ".:\ntotal 0").unwrap();
        for i in (0..60_000).rev() {
            writeln!(
                w,
                "-rw-r--r-- 1 u g {} Oct 1 12:00 f{i:08}",
                if after { 2 } else { 1 }
            )
            .unwrap();
        }
        if after {
            // After traversal order deliberately differs from lexical order.
            write!(w, "./z:\ntotal 1\n-rw-r--r-- 1 u g 200 Oct 1 12:00 tie\n./a:\ntotal 0\n-rw-r--r-- 1 u g 200 Oct 1 12:00 tie\n./empty-new:\ntotal 0\n").unwrap();
        } else {
            write!(w, "./gone:\ntotal 0\n-rw-r--r-- 1 u g 0 Oct 1 12:00 zero\n./a:\ntotal 0\n-rw-r--r-- 1 u g 100 Oct 1 12:00 tie\n./z:\ntotal 0\n-rw-r--r-- 1 u g 100 Oct 1 12:00 tie\n").unwrap();
        }
    }
    let output = tree.run(&["before", "after", "--top", "2", "--depth", "8"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Files: 60003 → 60002 | Directories: 4 → 4"));
    assert!(text.contains("Added 0 / removed 1 / resized 60002"));
    let increases = text
        .split("Largest file increases\n")
        .nth(1)
        .unwrap()
        .split("Largest file decreases")
        .next()
        .unwrap();
    assert!(increases.find("z/tie").unwrap() < increases.find("a/tie").unwrap());
    let largest = text.split("Largest current files").nth(1).unwrap();
    assert!(largest.find("z/tie").unwrap() < largest.find("a/tie").unwrap());
}

#[test]
fn duplicate_files_and_directories_across_runs_are_rejected_and_cleaned() {
    let tree = Tree::new();
    let path = tree.0.join("input");
    for duplicate in [
        "-rw-r--r-- 1 u g 1 Oct 1 12:00 f00000000\n",
        ".:\ntotal 0\n",
    ] {
        let mut w = BufWriter::new(File::create(&path).unwrap());
        write!(w, ".:\ntotal 0\n").unwrap();
        for i in 0..60_000 {
            writeln!(w, "-rw-r--r-- 1 u g 1 Oct 1 12:00 f{i:08}").unwrap();
        }
        write!(w, "{duplicate}").unwrap();
        w.flush().unwrap();
        let output = tree.run(&["input"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Duplicate"));
    }
}

#[test]
fn path_groups_survive_interleaved_prefixes_and_file_directory_replacement() {
    let tree = Tree::new();
    fs::write(tree.0.join("before"), ".:\ntotal 0\n-rw-r--r-- 1 u g 7 Oct 1 12:00 replacement\n./a:\ntotal 0\n-rw-r--r-- 1 u g 2 Oct 1 12:00 f\n./a-b:\ntotal 0\n-rw-r--r-- 1 u g 10 Oct 1 12:00 f\n./a/x:\ntotal 0\n-rw-r--r-- 1 u g 3 Oct 1 12:00 f\n").unwrap();
    fs::write(tree.0.join("after"), ".:\ntotal 0\ndrwxr-xr-x 1 u g 0 Oct 1 12:00 replacement\n./replacement:\ntotal 0\n-rw-r--r-- 1 u g 8 Oct 1 12:00 child\n./a/x:\ntotal 0\n-rw-r--r-- 1 u g 5 Oct 1 12:00 f\n./a-b:\ntotal 0\n-rw-r--r-- 1 u g 10 Oct 1 12:00 f\n./a:\ntotal 0\n-rw-r--r-- 1 u g 4 Oct 1 12:00 f\n").unwrap();
    let output = tree.run(&["before", "after"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Files: 4 → 4 | Directories: 4 → 5"));
    assert!(text.contains("Added 1 / removed 1 / resized 2"));
    assert!(
        text.lines()
            .any(|line| line.contains("9 B") && line.contains("+4 B") && line.ends_with("  a"))
    );
    assert!(text.contains("[removed] replacement"));
    assert!(text.contains("[added] replacement/child"));
}

#[test]
fn find_external_grouping_preserves_source_entry_order_and_parent_validation() {
    let tree = Tree::new();
    let record = |path: &str, kind: &str| {
        [
            path,
            kind,
            "1",
            "1",
            if kind == "d" {
                "drwxr-xr-x"
            } else {
                "-rw-r--r--"
            },
            "1",
            "1000",
            "1000",
            "1.0000000000",
            "",
        ]
        .join("\0")
            + "\0"
    };
    let mut w = BufWriter::new(File::create(tree.0.join("input.find")).unwrap());
    write!(w, "{}", record("", "d")).unwrap();
    // A parent is allowed to appear after its children in a saved stream.
    for i in (0..40_000).rev() {
        write!(w, "{}", record(&format!("parent/f{i:08}"), "f")).unwrap();
    }
    write!(w, "{}", record("parent", "d")).unwrap();
    w.flush().unwrap();
    let output = tree.run(&["export", "--input-format", "find", "input.find"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.find("f00039999").unwrap() < text.find("f00000000").unwrap());
    assert!(text.contains("total_bytes\t./parent\t\t\t\t\t\t\t\t\t\t20480000"));
    fs::write(
        tree.0.join("bad.find"),
        record("", "d") + &record("missing/file", "f"),
    )
    .unwrap();
    let bad = tree.run(&[
        "export",
        "--input-format",
        "find",
        "bad.find",
        "--output",
        "bad.tsv",
    ]);
    assert!(!bad.status.success());
    assert!(!tree.0.join("bad.tsv").exists());
}
