use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "find-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Tree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}
fn run(args: &[&str], path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args(args)
        .arg(path)
        .env("PATH", "")
        .output()
        .unwrap()
}
fn body(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .lines()
        .filter(|line| !line.starts_with("  after:") && !line.starts_with("  before:"))
        .collect::<Vec<_>>()
        .join("\n")
}
#[test]
fn saved_snapshots_work_without_find_or_the_original_filesystem() {
    for name in ["before", "after"] {
        let raw = fixture(&format!("{name}.find"));
        let expected = run(
            &["report", "--input-format", "tsv"],
            &fixture(&format!("{name}.find.tsv")),
        );
        for flags in [
            vec!["report", "--source", "find"],
            vec!["report", "--input-format", "find"],
            vec!["--source", "find"],
        ] {
            let actual = run(&flags, &raw);
            assert!(
                actual.status.success(),
                "{}",
                String::from_utf8_lossy(&actual.stderr)
            );
            assert_eq!(body(&actual.stdout), body(&expected.stdout));
        }
    }
    let output = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .env("PATH", "")
        .args([
            "report",
            "--input-format",
            "find",
            "--days",
            "3",
            "--depth",
            "2",
            "--top",
            "3",
            "--include-merged",
        ])
        .arg(fixture("before.find"))
        .arg(fixture("after.find"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Files: 4 → 5 | Directories: 3 → 3"));
    assert!(text.contains("Added 2 / removed 1 / resized 1 / same-size timestamp changes 1"));
    assert!(text.contains("ls block total: 8.00 KiB → 16.00 KiB (+8.00 KiB)"));
}
#[test]
fn raw_find_exports_round_trip_each_enabled_format() {
    let tree = Tree::new();
    for (format, enabled, binary) in [
        ("tsv", true, None),
        ("sqlite", cfg!(feature = "sqlite"), Some("sqlite3")),
        ("parquet", cfg!(feature = "parquet"), Some("duckdb")),
    ] {
        if !enabled
            || binary.is_some_and(|binary| Command::new(binary).arg("--version").output().is_err())
        {
            continue;
        }
        let output = tree.0.join(format!("snapshot.{format}"));
        let export = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args([
                "export",
                "--input-format",
                "find",
                "--format",
                format,
                "--output",
            ])
            .arg(&output)
            .arg(fixture("after.find"))
            .output()
            .unwrap();
        assert!(
            export.status.success(),
            "{}",
            String::from_utf8_lossy(&export.stderr)
        );
        let report = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["report", "--input-format", format])
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            report.status.success(),
            "{}",
            String::from_utf8_lossy(&report.stderr)
        );
        let expected = run(&["report", "--source", "find"], &fixture("after.find"));
        assert_eq!(body(&report.stdout), body(&expected.stdout));
    }
}
#[test]
fn collection_options_and_directories_are_rejected() {
    for flags in [
        vec!["--find-bin", "/missing"],
        vec!["--xdev"],
        vec!["--include-hidden"],
        vec!["--source", "unknown"],
        vec!["--source", "find", "--input-format", "tsv"],
    ] {
        assert!(!run(&flags, &fixture("after.find")).status.success());
    }
    assert!(!run(&["--source", "find"], Path::new(".")).status.success());
}
#[test]
fn malformed_snapshots_fail_without_publishing_an_artifact() {
    let tree = Tree::new();
    let input = tree.0.join("bad.find");
    let artifact = tree.0.join("out.tsv");
    let valid = fs::read(fixture("after.find")).unwrap();
    for raw in [
        Vec::new(),
        b"unterminated".to_vec(),
        valid[..valid.len() - 1].to_vec(),
        b"x\0f\0".to_vec(),
    ] {
        fs::write(&input, raw).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["export", "--source", "find", "--output"])
            .arg(&artifact)
            .arg(&input)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!artifact.exists());
    }
}
