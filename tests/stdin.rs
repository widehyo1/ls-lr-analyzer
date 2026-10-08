use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "stdin-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("tmp")).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str], data: Option<&[u8]>) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(args)
            .current_dir(&self.0)
            .env("TMPDIR", self.0.join("tmp"))
            .stdin(if data.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let output = if let Some(data) = data {
            let mut stdin = child.stdin.take().unwrap();
            std::thread::scope(|scope| {
                let writer = scope.spawn(move || stdin.write_all(data));
                let output = child.wait_with_output().unwrap();
                if let Err(error) = writer.join().unwrap() {
                    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
                }
                output
            })
        } else {
            child.wait_with_output().unwrap()
        };
        assert_eq!(fs::read_dir(self.0.join("tmp")).unwrap().count(), 0);
        assert!(!fs::read_dir(&self.0).unwrap().any(|p| {
            p.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".ls-lr-analyzer-")
        }));
        output
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
fn body(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.starts_with("  before:") && !l.starts_with("  after:"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn reports_match_file_input_and_label_stdin_for_all_text_formats() {
    let tree = Tree::new();
    for (format, name) in [
        ("ls", "after.txt"),
        ("find", "after.find"),
        ("tsv", "after.tsv"),
    ] {
        let path = fixture(name);
        let data = fs::read(&path).unwrap();
        let file = tree.run(
            &["report", "--input-format", format, path.to_str().unwrap()],
            None,
        );
        for args in [
            vec!["report", "--input-format", format, "-"],
            vec!["--input-format", format, "--", "-"],
        ] {
            let stdin = tree.run(&args, Some(&data));
            assert_eq!(body(&stdin), body(&file));
            assert!(String::from_utf8_lossy(&stdin.stdout).contains("  after: -\n"));
            assert_eq!(stdin.stderr, file.stderr);
        }
    }
    let source = tree.run(
        &["--source", "find", "-"],
        Some(&fs::read(fixture("after.find")).unwrap()),
    );
    assert!(source.status.success());
}

#[test]
fn comparison_accepts_stdin_on_either_side() {
    let tree = Tree::new();
    for (format, extension) in [("ls", "txt"), ("find", "find"), ("tsv", "tsv")] {
        let before = fixture(&format!("before.{extension}"));
        let after = fixture(&format!("after.{extension}"));
        let options = [
            "report",
            "--input-format",
            format,
            "--days",
            "3",
            "--depth",
            "2",
            "--top",
            "3",
        ];
        let mut args = options.to_vec();
        args.extend([before.to_str().unwrap(), after.to_str().unwrap()]);
        let expected = tree.run(&args, None);
        for old_stdin in [false, true] {
            let mut args = options.to_vec();
            args.extend(if old_stdin {
                ["-", after.to_str().unwrap()]
            } else {
                [before.to_str().unwrap(), "-"]
            });
            let data = fs::read(if old_stdin { &before } else { &after }).unwrap();
            let actual = tree.run(&args, Some(&data));
            assert_eq!(body(&actual), body(&expected));
            assert!(
                String::from_utf8_lossy(&actual.stdout).contains(if old_stdin {
                    "  before: -\n"
                } else {
                    "  after: -\n"
                })
            );
        }
    }
}

#[test]
fn exports_stream_stdin_to_stdout_and_file_without_overwriting() {
    let tree = Tree::new();
    for (format, name) in [("ls", "after.txt"), ("find", "after.find")] {
        let path = fixture(name);
        let data = fs::read(&path).unwrap();
        let expected = tree.run(
            &["export", "--input-format", format, path.to_str().unwrap()],
            None,
        );
        let actual = tree.run(&["export", "--input-format", format, "-"], Some(&data));
        assert!(actual.status.success());
        assert_eq!(actual.stdout, expected.stdout);
        assert_eq!(actual.stderr, expected.stderr);
        let destination = format!("{format}.tsv");
        let args = [
            "export",
            "--input-format",
            format,
            "-",
            "--output",
            &destination,
        ];
        let actual = tree.run(&args, Some(&data));
        assert!(actual.status.success());
        assert!(actual.stdout.is_empty());
        assert_eq!(
            fs::read(tree.0.join(&destination)).unwrap(),
            expected.stdout
        );
        let failed = tree.run(&args, Some(&data));
        assert!(!failed.status.success());
        assert!(String::from_utf8_lossy(&failed.stderr).contains("Output already exists"));
        assert_eq!(
            fs::read(tree.0.join(&destination)).unwrap(),
            expected.stdout
        );
    }
}

#[test]
fn rejects_double_stdin_binary_inputs_and_missing_input() {
    let tree = Tree::new();
    for args in [
        vec!["-", "-"],
        vec!["--input-format", "find", "-", "-"],
        vec!["--", "-", "-"],
    ] {
        let output = tree.run(&args, None);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Only one input may read stdin"));
    }
    for format in ["sqlite", "duckdb", "parquet"] {
        let output = tree.run(&["report", "--input-format", format, "-"], None);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("requires a file path"));
    }
    let output = tree.run(&[], Some(b".:\ntotal 0\n"));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Expected AFTER"));
    // A literal file named '-' is available through an explicit relative path.
    fs::write(tree.0.join("-"), ".:\ntotal 0\n").unwrap();
    assert!(tree.run(&["./-"], None).status.success());
}

#[test]
fn malformed_stdin_fails_and_removes_temporary_artifacts() {
    let tree = Tree::new();
    for (format, data) in [
        ("ls", b"".as_slice()),
        ("find", b"".as_slice()),
        ("find", b"unterminated".as_slice()),
        ("tsv", b"bad header\n".as_slice()),
    ] {
        let output = tree.run(&["report", "--input-format", format, "-"], Some(data));
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    let mut truncated = fs::read(fixture("after.find")).unwrap();
    truncated.pop();
    let output = tree.run(
        &[
            "export",
            "--input-format",
            "find",
            "-",
            "--output",
            "bad.tsv",
        ],
        Some(&truncated),
    );
    assert!(!output.status.success());
    assert!(!tree.0.join("bad.tsv").exists());
}

#[test]
fn stdin_supports_enabled_database_exports() {
    let tree = Tree::new();
    for (format, binary, enabled) in [
        ("sqlite", "sqlite3", cfg!(feature = "sqlite")),
        ("parquet", "duckdb", cfg!(feature = "parquet")),
    ] {
        if !enabled || Command::new(binary).arg("--version").output().is_err() {
            continue;
        }
        for (source, name) in [("ls", "after.txt"), ("find", "after.find")] {
            let path = fixture(name);
            let data = fs::read(&path).unwrap();
            let destination = format!("{source}.{format}");
            let output = tree.run(
                &[
                    "export",
                    "--input-format",
                    source,
                    "-",
                    "--format",
                    format,
                    "--output",
                    &destination,
                ],
                Some(&data),
            );
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let report = tree.run(&["report", "--input-format", format, &destination], None);
            let expected = tree.run(
                &["report", "--input-format", source, path.to_str().unwrap()],
                None,
            );
            assert_eq!(body(&report), body(&expected));
        }
    }
}
