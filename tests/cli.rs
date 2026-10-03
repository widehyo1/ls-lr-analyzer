use std::path::PathBuf;
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn tsv_export_has_fixed_columns_and_original_sizes() {
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args(["--format", "tsv"])
        .arg(fixture("after.txt"))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.lines().count(), 12);
    assert!(text.lines().all(|line| line.split('\t').count() == 12));
    assert!(
        text.contains("entry\t./growing\t-rw-r--r--\t1\tu\tg\t0\tOct\t1\t12:00\tzero size.txt\t\n")
    );
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args(["--format", "tsv"])
        .arg(fixture("before.txt"))
        .arg(fixture("after.txt"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("requires one input"));
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

#[test]
fn help_and_version_work_without_inputs_for_each_command() {
    for command in [None, Some("export"), Some("report")] {
        for flag in ["-V", "--version", "-h", "--help"] {
            let mut process = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"));
            if let Some(command) = command {
                process.arg(command);
            }
            let out = process.arg(flag).output().unwrap();
            assert!(out.status.success());
            assert!(out.stderr.is_empty());
            let text = String::from_utf8(out.stdout).unwrap();
            if matches!(flag, "-V" | "--version") {
                assert_eq!(
                    text,
                    format!("ls-lr-analyzer {}\n", env!("CARGO_PKG_VERSION"))
                );
            } else {
                assert!(text.contains("--input-format"));
            }
        }
    }
}

#[test]
fn export_defaults_to_tsv_and_report_retains_legacy_behavior() {
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .arg("export")
        .arg(fixture("before.txt"))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, std::fs::read(fixture("before.tsv")).unwrap());
    for inputs in [
        vec![fixture("after.txt")],
        vec![fixture("before.txt"), fixture("after.txt")],
    ] {
        let legacy = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(&inputs)
            .output()
            .unwrap();
        let report = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .arg("report")
            .args(&inputs)
            .output()
            .unwrap();
        assert!(legacy.status.success() && report.status.success());
        assert_eq!(report.stdout, legacy.stdout);
    }
}

#[test]
fn invalid_option_values_and_command_combinations_fail() {
    for flags in [
        vec!["export", "--format", "text"],
        vec!["export", "--top", "3"],
        vec!["export", "--input-format", "tsv"],
        vec!["report", "--format", "tsv"],
        vec!["report", "--input-format", "unknown"],
        vec!["report", "--output", "unused.txt"],
        vec!["report", "--days", "1"],
        vec!["report", "--top", "0"],
        vec!["report", "--depth", "0"],
        vec!["report", "--block-size", "0"],
        vec!["export", "--format", "unknown"],
        vec!["export", "--format", "duckdb"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(&flags)
            .arg(fixture("after.txt"))
            .output()
            .unwrap();
        assert!(!out.status.success(), "{flags:?}");
        assert!(out.stdout.is_empty());
        assert!(!out.stderr.is_empty());
    }
    for days in ["0", "-1", "NaN", "inf", "invalid"] {
        let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .arg("report")
            .arg(fixture("before.txt"))
            .arg(fixture("after.txt"))
            .args(["--days", days])
            .output()
            .unwrap();
        assert!(!out.status.success(), "--days {days}");
    }
    for flag in [
        "--format",
        "--input-format",
        "--output",
        "--sqlite3-bin",
        "--duckdb-bin",
        "--top",
        "--depth",
        "--block-size",
        "--days",
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .arg(fixture("after.txt"))
            .arg(flag)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{flag}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("requires"));
    }
}
