use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn compares_reordered_directories_and_different_root_names() {
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .arg(fixture("before.txt"))
        .arg(fixture("after.txt"))
        .args(["--top", "3", "--depth", "2", "--days", "3"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.chars().any(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c)));
    assert!(text.contains("Files: 4 → 5 | Directories: 3 → 3"));
    assert!(text.contains("Added 2 / removed 1 / resized 1 / same-size timestamp changes 1"));
    assert!(text.contains(
        "new files 450 B + existing growth 1000 B - removed 300 B - existing shrinkage 0 B"
    ));
    assert!(text.contains("ls block total: 8.00 KiB → 16.00 KiB (+8.00 KiB)"));
    assert!(text.contains("[resized] growing/database.db"));
    assert!(!text.contains("Warning"));
}

#[test]
fn single_snapshot_has_capacity_rankings_without_comparison_claims() {
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .arg(fixture("after.txt"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.chars().any(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c)));
    assert!(text.contains("Files 5 | Directories 3"));
    assert!(text.contains("Largest directories"));
    assert!(text.contains("growing/database.db"));
    assert!(!text.contains("Largest file increases"));
}

#[test]
fn invalid_cli_options_fail_with_explanation() {
    for flag in ["--top", "--depth", "--block-size"] {
        let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .arg(fixture("after.txt"))
            .args([flag, "0"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("positive"));
    }
}
