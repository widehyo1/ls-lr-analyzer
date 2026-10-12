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
    let limits = dir.0.join("limits");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nulimit -v >> '{}'\nexec '{}' \"$@\"\n",
            log.display(),
            limits.display(),
            binary.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let long = format!(
        ".:\ntotal 0\n-rw-r--r-- 1 u g 1 Oct 3 12:00 {}\n",
        "a".repeat(200 * 1024)
    );
    for (name, data, rows) in [
        ("empty", "", "0"),
        (
            "special",
            "./data:\ntotal 0\n-rw-r--r-- 1 NULL g 0 Oct 3 12:00 한글\tfile\\name\0x\n",
            "3",
        ),
        ("long", long.as_str(), "3"),
    ] {
        let input = dir.0.join(name);
        fs::write(&input, data).unwrap();
        let output = dir.0.join(format!("{name}.parquet"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"));
        command
            .args(["export", "--format", "parquet", "--duckdb-bin"])
            .arg(&wrapper)
            .arg(&input)
            .arg("--output")
            .arg(&output);
        if name == "special" {
            command.args(["--duckdb-memory-limit", "128MiB"]);
        }
        let exported = command.output().unwrap();
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
                .args([
                    "report",
                    "--input-format",
                    "parquet",
                    "--duckdb-memory-limit",
                    "256MiB",
                    "--duckdb-bin",
                ])
                .arg(&wrapper)
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
    assert!(!commands.contains("buffer_size="));
    assert!(commands.contains("memory_limit='67108864B'"));
    assert!(commands.contains("memory_limit='134217728B'"));
    assert!(commands.contains("memory_limit='268435456B'"));
    let limits = fs::read_to_string(limits).unwrap();
    assert!(limits.lines().all(|line| {
        line.parse::<u64>()
            .is_ok_and(|n| n > 0 && n <= 2 * 1024 * 1024)
    }));
}

#[cfg(all(unix, feature = "parquet"))]
#[test]
fn stdin_parquet_batches_preserve_all_cells_and_row_order() {
    use std::io::Write;
    use std::process::Stdio;
    if Command::new("duckdb").arg("--version").output().is_err() {
        return;
    }
    let dir = TestDir::new();
    let output = dir.0.join("many.parquet");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args([
            "export",
            "--format",
            "parquet",
            "--duckdb-memory-limit",
            "64MiB",
            "--output",
        ])
        .arg(&output)
        .arg("-")
        .env("TMPDIR", &dir.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "./data:\ntotal 0").unwrap();
    for i in 0..20000 {
        writeln!(
            stdin,
            "-rw-r--r-- 1 NULL g {i} Oct 3 12:00 한글\tfile\\name-{i}"
        )
        .unwrap();
    }
    drop(stdin);
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stdout.is_empty());
    let query = format!(
        "SELECT count(*), sum(row_index), count(CASE WHEN record='entry' AND owner='NULL' AND size_or_device=CAST(row_index-3 AS VARCHAR) AND name='한글' || chr(9) || 'file\\name-' || CAST(row_index-3 AS VARCHAR) THEN 1 END) FROM read_parquet('{}'); SELECT count(DISTINCT row_group_id) FROM parquet_metadata('{}');",
        output.display(),
        output.display()
    );
    let counted = Command::new("duckdb")
        .args([
            "-batch",
            "-bail",
            "-init",
            "/dev/null",
            "-csv",
            "-noheader",
            ":memory:",
        ])
        .arg(query)
        .output()
        .unwrap();
    assert!(
        counted.status.success(),
        "{}",
        String::from_utf8_lossy(&counted.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&counted.stdout).trim(),
        "20002,200050003,20000\n3"
    );
    let ordered = Command::new("duckdb").args(["-batch", "-bail", "-init", "/dev/null", "-csv", "-noheader", ":memory:"]).arg(format!("SELECT count(*) FROM (SELECT row_index, row_number() OVER () AS physical FROM read_parquet('{}')) WHERE row_index<>physical;", output.display())).output().unwrap();
    assert!(ordered.status.success());
    assert_eq!(String::from_utf8_lossy(&ordered.stdout).trim(), "0");
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[cfg(all(unix, feature = "parquet"))]
#[test]
fn insufficient_duckdb_budget_does_not_publish_partial_output() {
    if Command::new("duckdb").arg("--version").output().is_err() {
        return;
    }
    let dir = TestDir::new();
    let output = dir.0.join("failed.parquet");
    let result = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args([
            "export",
            "--format",
            "parquet",
            "--duckdb-memory-limit",
            "1B",
            "--output",
        ])
        .arg(&output)
        .arg(fixture())
        .env("TMPDIR", &dir.0)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[cfg(all(target_os = "linux", feature = "parquet"))]
#[test]
fn malformed_stdin_after_a_completed_batch_removes_all_staging() {
    use std::io::Write;
    use std::process::Stdio;
    if Command::new("duckdb").arg("--version").output().is_err() {
        return;
    }
    let dir = TestDir::new();
    let output = dir.0.join("failed.parquet");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args(["export", "--format", "parquet", "--output"])
        .arg(&output)
        .arg("-")
        .env("TMPDIR", &dir.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, ".:\ntotal 0").unwrap();
    for i in 0..10000 {
        writeln!(stdin, "-rw-r--r-- 1 u g 1 Oct 3 12:00 f{i}").unwrap();
    }
    let _ = stdin.write_all(b"\xff\n");
    drop(stdin);
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[cfg(all(target_os = "linux", feature = "parquet"))]
#[test]
fn duckdb_child_address_space_is_enforced_outside_the_buffer_manager() {
    use std::os::unix::fs::PermissionsExt;
    if Command::new("python3").arg("--version").output().is_err() {
        return;
    }
    let dir = TestDir::new();
    let backend = dir.0.join("allocate-address-space");
    // mmap reserves virtual space without touching 2 GiB of physical RAM.
    // This exercises the OS guard, independent of DuckDB's managed allocator.
    fs::write(&backend, "#!/usr/bin/env python3\nimport mmap,sys\ntry:\n    allocation=mmap.mmap(-1, 2*1024*1024*1024)\nexcept (OSError,MemoryError):\n    print('address-space guard enforced',file=sys.stderr)\n    sys.exit(1)\nprint(42)\n").unwrap();
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o755)).unwrap();
    let output = dir.0.join("failed.parquet");
    let result = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args([
            "export",
            "--format",
            "parquet",
            "--duckdb-memory-limit",
            "64MiB",
            "--duckdb-bin",
        ])
        .arg(&backend)
        .arg("--output")
        .arg(&output)
        .arg(fixture())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("address-space guard enforced"));
    assert!(!output.exists());
    let result = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args(["report", "--input-format", "parquet", "--duckdb-bin"])
        .arg(&backend)
        .arg(fixture())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("address-space guard enforced"));
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}

#[cfg(all(target_os = "linux", feature = "parquet"))]
#[test]
fn successful_client_status_with_incomplete_rows_is_rejected() {
    use std::os::unix::fs::PermissionsExt;
    if Command::new("python3").arg("--version").output().is_err() {
        return;
    }
    let dir = TestDir::new();
    let backend = dir.0.join("partial-client");
    fs::write(
        &backend,
        r#"#!/usr/bin/env python3
import sys,re,pathlib
output=re.search(r" TO '([^']+)'",sys.argv[-1]).group(1)
rows=[]
for index,record in [(1,'directory'),(2,'total_bytes')]:
    cells=[record,'.']+['']*10
    if record=='total_bytes': cells[-1]='0'
    rows.append(str(index)+'\t'+'\t'.join(c.encode().hex() for c in cells)+'\n')
pathlib.Path(output).write_text(''.join(rows))
print(3)
"#,
    )
    .unwrap();
    fs::set_permissions(&backend, fs::Permissions::from_mode(0o755)).unwrap();
    let temporary = dir.0.join("tmp");
    fs::create_dir(&temporary).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .env("TMPDIR", &temporary)
        .args(["report", "--input-format", "parquet", "--duckdb-bin"])
        .arg(&backend)
        .arg(fixture())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("expected 3 rows, received 2"));
    assert_eq!(fs::read_dir(temporary).unwrap().count(), 0);
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

#[cfg(all(feature = "sqlite", feature = "parquet"))]
#[test]
fn shuffled_parquet_to_sqlite_preserves_cells_reports_and_atomicity() {
    if Command::new("duckdb").arg("--version").output().is_err()
        || Command::new("sqlite3").arg("--version").output().is_err()
    {
        return;
    }
    let dir = TestDir::new();
    let parquet = dir.0.join("shuffled.parquet");
    let made = Command::new("duckdb").args(["-batch", "-bail", ":memory:"])
        .arg(format!("COPY (SELECT row_index - 3 AS row_index, * EXCLUDE (row_index) FROM read_parquet('{}') ORDER BY row_index DESC) TO '{}' (FORMAT PARQUET);",
            named_fixture("after.parquet").display(), parquet.display()))
        .output().unwrap();
    assert!(made.status.success());
    let sqlite = dir.0.join("snapshot.sqlite");
    let too_small = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args([
            "export",
            "--input-format",
            "parquet",
            "--format",
            "sqlite",
            "--duckdb-memory-limit",
            "1B",
            "--output",
        ])
        .arg(&sqlite)
        .arg(&parquet)
        .output()
        .unwrap();
    assert!(!too_small.status.success());
    assert!(!sqlite.exists());
    let export = || {
        Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args([
                "export",
                "--input-format",
                "parquet",
                "--format",
                "sqlite",
                "--duckdb-memory-limit",
                "64MiB",
                "--output",
            ])
            .arg(&sqlite)
            .arg(&parquet)
            .output()
            .unwrap()
    };
    let result = export();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let expected = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .arg(named_fixture("after.txt"))
        .output()
        .unwrap();
    for (format, input) in [("parquet", &parquet), ("sqlite", &sqlite)] {
        let report = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["report", "--input-format", format])
            .arg(input)
            .output()
            .unwrap();
        assert!(
            report.status.success(),
            "{}",
            String::from_utf8_lossy(&report.stderr)
        );
        assert_eq!(report_body(&report.stdout), report_body(&expected.stdout));
    }
    let columns = [
        "record",
        "directory",
        "permissions",
        "links",
        "owner",
        "group",
        "size_or_device",
        "month",
        "day",
        "time_or_year",
        "name",
        "total",
    ]
    .map(|c| format!("hex(\"{c}\")"))
    .join(" || ':' || ");
    let original = Command::new("duckdb")
        .args(["-list", "-noheader", ":memory:"])
        .arg(format!(
            "SELECT {columns} FROM read_parquet('{}') ORDER BY row_index;",
            parquet.display()
        ))
        .output()
        .unwrap();
    let converted = Command::new("sqlite3")
        .args(["-list", "-noheader"])
        .arg(&sqlite)
        .arg(format!(
            "SELECT {columns} FROM snapshot ORDER BY row_index;"
        ))
        .output()
        .unwrap();
    assert!(original.status.success() && converted.status.success());
    assert_eq!(original.stdout, converted.stdout);
    let bytes = fs::read(&sqlite).unwrap();
    assert!(!export().status.success());
    assert_eq!(fs::read(&sqlite).unwrap(), bytes);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);
}

#[cfg(feature = "sqlite")]
#[test]
fn sqlite_read_rejects_rows_beyond_engine_budget_and_cleans_staging() {
    if Command::new("sqlite3").arg("--version").output().is_err() {
        return;
    }
    let dir = TestDir::new();
    let database = dir.0.join("huge.sqlite");
    let schema = fs::read_to_string(named_fixture("after.tsv")).unwrap();
    let columns = schema
        .lines()
        .next()
        .unwrap()
        .split('\t')
        .map(|c| format!("\"{c}\" TEXT"))
        .collect::<Vec<_>>()
        .join(", ");
    let made = Command::new("sqlite3").arg(&database)
        .arg(format!("CREATE TABLE snapshot (row_index BIGINT, {columns}); INSERT INTO snapshot (row_index, record) VALUES (1, zeroblob(80*1024*1024));"))
        .output().unwrap();
    assert!(made.status.success());
    let temporary = dir.0.join("tmp");
    fs::create_dir(&temporary).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .env("TMPDIR", &temporary)
        .args(["report", "--input-format", "sqlite"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("out of memory"));
    assert!(out.stdout.is_empty());
    assert_eq!(fs::read_dir(&temporary).unwrap().count(), 0);
    let updated = Command::new("sqlite3")
        .arg(&database)
        .arg("UPDATE snapshot SET record=NULL;")
        .output()
        .unwrap();
    assert!(updated.status.success());
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .env("TMPDIR", &temporary)
        .args(["report", "--input-format", "sqlite"])
        .arg(&database)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Invalid hexadecimal snapshot cell"));
    assert_eq!(fs::read_dir(temporary).unwrap().count(), 0);
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
        fs::write(&path, "#!/bin/sh\ncase \"$*\" in\n*'PRAGMA hard_heap_limit; SELECT 42;'*) printf '67108864\\n42\\n' ;;\n*'SELECT 42;'*) printf '42\\n' ;;\n*) printf 'intentional backend failure\\n' >&2; exit 1 ;;\nesac\n").unwrap();
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
