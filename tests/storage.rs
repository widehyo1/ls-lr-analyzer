use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("ls-lr-storage-test-{}-{id}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn formats() -> Vec<(&'static str, &'static str, bool)> {
    vec![
        ("sqlite", "sqlite3", cfg!(feature = "sqlite")),
        ("parquet", "duckdb", cfg!(feature = "parquet")),
    ]
}

#[cfg(all(unix, feature = "parquet"))]
#[test]
fn parquet_uses_memory_bulk_import_and_preserves_empty_and_special_cells() {
    use std::os::unix::fs::PermissionsExt;
    let Some(binary) = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join("duckdb"))
        .find(|p| p.is_file())
    else {
        return;
    };
    let dir = TestDir::new();
    let wrapper = dir.0.join("duckdb-wrapper");
    let log = dir.0.join("arguments");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            binary.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    for (name, data, rows) in [
        ("empty", "", "0"),
        (
            "special",
            "./data:\ntotal 0\n-rw-r--r-- 1 NULL g 0 Oct 3 12:00 한글\tfile\\name\0x\n",
            "3",
        ),
    ] {
        let input = dir.0.join(name);
        fs::write(&input, data).unwrap();
        let output = dir.0.join(format!("{name}.parquet"));
        let exported = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["export", "--format", "parquet", "--duckdb-bin"])
            .arg(&wrapper)
            .arg(&input)
            .arg("--output")
            .arg(&output)
            .output()
            .unwrap();
        assert!(
            exported.status.success(),
            "{}",
            String::from_utf8_lossy(&exported.stderr)
        );
        let counted = Command::new(&binary)
            .args(["-csv", "-noheader", ":memory:"])
            .arg(format!(
                "SELECT COUNT(*) FROM read_parquet('{}');",
                output.display()
            ))
            .output()
            .unwrap();
        assert!(counted.status.success());
        assert_eq!(String::from_utf8_lossy(&counted.stdout).trim(), rows);
        if rows != "0" {
            let report = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
                .args(["report", "--input-format", "parquet"])
                .arg(&output)
                .output()
                .unwrap();
            assert!(report.status.success());
            assert!(String::from_utf8_lossy(&report.stdout).contains("Files 1"));
        }
    }
    let commands = fs::read_to_string(log).unwrap();
    assert!(commands.contains(":memory:\n"));
    assert!(commands.contains("read_csv("));
    assert!(!commands.contains("INSERT"));
    assert!(!commands.contains("snapshot.db"));
}

#[test]
fn file_export_announces_start_and_success_without_polluting_stdout() {
    let dir = TestDir::new();
    let input = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/after.txt");
    let mut enabled = vec!["tsv"];
    enabled.extend(
        formats()
            .into_iter()
            .filter(|(_, binary, feature)| {
                *feature
                    && Command::new(binary)
                        .arg("--version")
                        .output()
                        .is_ok_and(|o| o.status.success())
            })
            .map(|(format, _, _)| format),
    );
    for format in enabled {
        let output = dir.0.join(format!("snapshot.{format}"));
        let result = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["export", "--format", format, "--output"])
            .arg(&output)
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty());
        let messages = String::from_utf8_lossy(&result.stderr);
        assert!(
            messages.starts_with(&format!("started: export {format} -> ")),
            "{format}: {messages:?}"
        );
        assert!(
            messages
                .lines()
                .last()
                .unwrap()
                .starts_with(&format!("finished: export {format} -> "))
        );
        let failed = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["export", "--format", format, "--output"])
            .arg(&output)
            .arg(&input)
            .output()
            .unwrap();
        assert!(!failed.status.success());
        let messages = String::from_utf8_lossy(&failed.stderr);
        assert!(messages.starts_with("started:"));
        assert!(!messages.contains("finished:"));
    }
    let stdout_export = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .arg("export")
        .arg(&input)
        .output()
        .unwrap();
    assert!(stdout_export.status.success());
    assert!(stdout_export.stderr.is_empty());
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/after.txt")
}

fn named_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn report_body(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("  before:") && !line.starts_with("  after:"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn all_fixture_formats_produce_the_same_capacity_and_comparison_reports() {
    let mut supported = vec![("tsv", "", true)];
    supported.extend(formats());
    supported.push(("duckdb", "duckdb", cfg!(feature = "duckdb")));
    for (format, binary, enabled) in supported {
        if !enabled {
            continue;
        }
        if !binary.is_empty() && Command::new(binary).arg("--version").output().is_err() {
            eprintln!("Skipping {format} fixture reports: {binary} is unavailable");
            continue;
        }
        for compare in [false, true] {
            for options in [
                vec![],
                vec!["--depth", "2", "--top", "3"],
                vec!["--block-size", "512", "--include-merged"],
                vec![
                    "--depth",
                    "2",
                    "--top",
                    "3",
                    "--block-size",
                    "512",
                    "--include-merged",
                ],
            ] {
                let mut args = options.clone();
                if compare {
                    args.extend(["--days", "3"]);
                }
                let mut original = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"));
                let mut artifact = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"));
                original.arg("report");
                artifact.args(["report", "--input-format", format]);
                if compare {
                    original.arg(named_fixture("before.txt"));
                    artifact.arg(named_fixture(&format!("before.{format}")));
                }
                original.arg(named_fixture("after.txt")).args(&args);
                artifact
                    .arg(named_fixture(&format!("after.{format}")))
                    .args(&args);
                let expected = original.output().unwrap();
                let actual = artifact.output().unwrap();
                assert!(expected.status.success());
                assert!(
                    actual.status.success(),
                    "{format}: {}",
                    String::from_utf8_lossy(&actual.stderr)
                );
                assert_eq!(
                    report_body(&actual.stdout),
                    report_body(&expected.stdout),
                    "{format}: {args:?}"
                );
                assert_eq!(actual.stderr, expected.stderr);
            }
        }
    }
}

#[test]
fn tsv_file_export_and_positional_marker_round_trip() {
    let dir = TestDir::new();
    let input = dir.0.join("--version");
    fs::copy(fixture(), &input).unwrap();
    let output = dir.0.join("snapshot.tsv");
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .current_dir(&dir.0)
        .args(["export", "--output", "snapshot.tsv", "--", "--version"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert_eq!(
        fs::read(&output).unwrap(),
        fs::read(named_fixture("after.tsv")).unwrap()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args(["report", "--input-format", "tsv"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(out.status.success());
    let retry = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args(["export", "--output"])
        .arg(&output)
        .arg(&input)
        .output()
        .unwrap();
    assert!(!retry.status.success());
}

#[test]
fn malformed_tsv_and_missing_artifacts_are_rejected() {
    let dir = TestDir::new();
    let valid = fs::read_to_string(named_fixture("after.tsv")).unwrap();
    for contents in [
        "wrong header\n".to_owned(),
        format!("{}\nentry\ttoo few\n", valid.lines().next().unwrap()),
        valid.replace("stable.txt", "bad\\q"),
        valid.replacen("directory\t.", "unknown\t.", 1),
        valid.replacen("entry\t.", "entry\twrong", 1),
    ] {
        let input = dir.0.join("bad.tsv");
        fs::write(&input, contents).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["report", "--input-format", "tsv"])
            .arg(input)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
    }
    for (format, _, enabled) in formats() {
        if !enabled {
            continue;
        }
        let input = dir.0.join(format!("missing.{format}"));
        let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["report", "--input-format", format])
            .arg(&input)
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(!input.exists());
    }
}

#[test]
fn tsv_errors_do_not_publish_partial_output() {
    let dir = TestDir::new();
    let input = dir.0.join("input.txt");
    let output = dir.0.join("output.tsv");
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args(["export", "--output"])
        .arg(&output)
        .arg(&input)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(fs::read_dir(&dir.0).unwrap().next().is_none());
    fs::write(&input, b".:\ntotal 0\n\xff\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args(["export", "--output"])
        .arg(&output)
        .arg(&input)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[test]
fn explicit_binary_paths_work_for_reports_without_path_and_leave_inputs_unchanged() {
    let dir = TestDir::new();
    let mut readable = formats();
    readable.push(("duckdb", "duckdb", cfg!(feature = "duckdb")));
    for (format, binary, enabled) in readable {
        if !enabled {
            continue;
        }
        let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|p| p.join(binary))
            .find(|p| p.is_file());
        let Some(executable) = executable else {
            continue;
        };
        let executable = fs::canonicalize(executable).unwrap();
        let flag = if format == "sqlite" {
            "--sqlite3-bin"
        } else {
            "--duckdb-bin"
        };
        let before = named_fixture(&format!("before.{format}"));
        let after = named_fixture(&format!("after.{format}"));
        let before_bytes = fs::read(&before).unwrap();
        let after_bytes = fs::read(&after).unwrap();
        let expected = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .arg("report")
            .arg(named_fixture("before.txt"))
            .arg(named_fixture("after.txt"))
            .args(["--top", "3", "--depth", "2", "--days", "0.5"])
            .output()
            .unwrap();
        let actual = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .env("PATH", &dir.0)
            .args(["report", "--input-format", format, flag])
            .arg(executable)
            .arg(&before)
            .arg(&after)
            .args(["--top", "3", "--depth", "2", "--days", "0.5"])
            .output()
            .unwrap();
        assert!(
            actual.status.success(),
            "{}",
            String::from_utf8_lossy(&actual.stderr)
        );
        assert_eq!(report_body(&actual.stdout), report_body(&expected.stdout));
        assert_eq!(before_bytes, fs::read(before).unwrap());
        assert_eq!(after_bytes, fs::read(after).unwrap());
    }
}

#[test]
fn tsv_report_preserves_diagnostics_metadata_and_overlay_filtering() {
    let dir = TestDir::new();
    let input = dir.0.join("input.txt");
    let artifact = dir.0.join("snapshot.tsv");
    fs::write(&input, ".:\n합계 4\n-rw-r--r-- 1 u g 42 Sep 28 2025 한글\tfile\\t:\nlrwxrwxrwx 1 u g 6 Sep 28 2025 link -> target\nbad input\nls: permission denied\n./containers/storage/overlay/id/merged:\ntotal 4\n-rw-r--r-- 1 u g 99 Oct 1 12:00 duplicate\n./empty:\n").unwrap();
    let export = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args(["export", "--output"])
        .arg(&artifact)
        .arg(&input)
        .output()
        .unwrap();
    assert!(export.status.success());
    for flags in [
        vec![],
        vec!["--include-merged", "--block-size", "512", "--top", "1"],
    ] {
        let expected = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .arg("report")
            .arg(&input)
            .args(&flags)
            .output()
            .unwrap();
        let actual = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["report", "--input-format", "tsv"])
            .arg(&artifact)
            .args(&flags)
            .output()
            .unwrap();
        assert!(expected.status.success() && actual.status.success());
        assert_eq!(report_body(&actual.stdout), report_body(&expected.stdout));
        assert_eq!(actual.stderr, expected.stderr);
    }
}

#[test]
fn file_exports_require_a_destination_and_an_enabled_feature() {
    let dir = TestDir::new();
    for (format, _, enabled) in formats() {
        let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["--format", format])
            .arg(fixture())
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("--output FILE is required"));
        if !enabled {
            let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
                .args(["--format", format, "--output"])
                .arg(dir.0.join(format))
                .arg(fixture())
                .output()
                .unwrap();
            assert!(!out.status.success());
            assert!(String::from_utf8_lossy(&out.stderr).contains("reinstall with --features"));
            let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
                .args(["report", "--input-format", format])
                .arg(named_fixture(&format!("after.{format}")))
                .output()
                .unwrap();
            assert!(!out.status.success());
            assert!(String::from_utf8_lossy(&out.stderr).contains("reinstall with --features"));
        }
    }
    assert!(fs::read_dir(&dir.0).unwrap().next().is_none());
}

#[test]
fn unavailable_tools_fail_without_leaving_artifacts() {
    let dir = TestDir::new();
    for (format, binary, enabled) in formats() {
        if !enabled {
            continue;
        }
        let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .env("PATH", &dir.0)
            .args(["--format", format, "--output"])
            .arg(dir.0.join(format))
            .arg(fixture())
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains(&format!("Cannot execute {binary}")));
        assert!(fs::read_dir(&dir.0).unwrap().next().is_none());
    }
}

#[test]
fn installed_clients_round_trip_metadata_and_refuse_overwrites() {
    let dir = TestDir::new();
    let input = dir.0.join("input.txt");
    fs::write(&input, "/data:\ntotal 4\n-rw-r--r-- 1 u g 18446744073709551615 Oct 1 12:00 odd'\t\\한글\nlrwxrwxrwx 1 u g 6 Sep 28 2025 link -> target\ncrw-rw-rw- 1 root root 1,   3 Oct 1 12:00 null\n/data/empty:\ntotal 0\nls: permission denied\nbad input\n").unwrap();
    let init = dir.0.join("init.sql");
    fs::write(&init, "").unwrap();
    for (format, binary, enabled) in formats() {
        if !enabled {
            continue;
        }
        let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|p| p.join(binary))
            .find(|p| p.is_file());
        let Some(executable) = executable else {
            eprintln!("Skipping {format} round trip: {binary} is unavailable");
            continue;
        };
        let executable = fs::canonicalize(executable).unwrap();
        let binary_flag = if format == "sqlite" {
            "--sqlite3-bin"
        } else {
            "--duckdb-bin"
        };
        // The apostrophe in the parent path also tests SQL path quoting.
        let parent = dir.0.join(format!("{format}' path"));
        fs::create_dir(&parent).unwrap();
        let output = parent.join("snapshot");
        let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .env("PATH", dir.0.join("missing-path"))
            .arg(binary_flag)
            .arg(&executable)
            .args(["--format", format, "--output"])
            .arg(&output)
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{format}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains("2 ls error or unparsed lines"));
        let table = if format == "parquet" {
            format!(
                "read_parquet('{}')",
                output.to_str().unwrap().replace('\'', "''")
            )
        } else {
            "snapshot".into()
        };
        let query = format!(
            "SELECT count(*), count(CASE WHEN record='entry' THEN 1 END) FROM {table}; SELECT CASE WHEN name='odd''\t\\한글' AND size_or_device='18446744073709551615' THEN 42 ELSE 0 END FROM {table} WHERE row_index=3; SELECT CASE WHEN size_or_device='1,3' THEN 42 ELSE 0 END FROM {table} WHERE row_index=5;"
        );
        let database = if format == "parquet" {
            PathBuf::from(":memory:")
        } else {
            output.clone()
        };
        let read = Command::new(&executable)
            .args(["-batch", "-bail", "-init"])
            .arg(&init)
            .args(["-csv", "-noheader"])
            .arg(database)
            .arg(query)
            .output()
            .unwrap();
        assert!(
            read.status.success(),
            "{}",
            String::from_utf8_lossy(&read.stderr)
        );
        assert_eq!(
            String::from_utf8(read.stdout)
                .unwrap()
                .replace("\r\n", "\n"),
            "9,3\n42\n42\n"
        );
        let bytes = fs::read(&output).unwrap();
        let retry = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["--format", format, "--output"])
            .arg(&output)
            .arg(&input)
            .output()
            .unwrap();
        assert!(!retry.status.success());
        assert!(String::from_utf8_lossy(&retry.stderr).contains("Output already exists"));
        assert_eq!(fs::read(&output).unwrap(), bytes);
        assert_eq!(fs::read_dir(parent).unwrap().count(), 1);
    }
}

#[cfg(unix)]
#[test]
fn client_errors_are_reported_and_partial_exports_are_removed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TestDir::new();
    let binaries = dir.0.join("bin");
    fs::create_dir(&binaries).unwrap();
    for binary in ["sqlite3", "duckdb"] {
        let path = binaries.join(binary);
        fs::write(&path, "#!/bin/sh\ncase \"$*\" in\n*'SELECT 42;'*) printf '42\\n' ;;\n*) printf 'intentional backend failure\\n' >&2; exit 1 ;;\nesac\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    for (format, _, enabled) in formats() {
        if !enabled {
            continue;
        }
        let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .env("PATH", &binaries)
            .args(["--format", format, "--output"])
            .arg(dir.0.join(format))
            .arg(fixture())
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("intentional backend failure"));
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }
}
