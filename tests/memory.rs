use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn oversized_stdin_is_rejected_without_waiting_for_eof() {
    for (format, length) in [("find", 64 * 1024 + 1), ("ls", 256 * 1024 + 1)] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args(["export", "--input-format", format, "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let _ = stdin.write_all(&vec![b'a'; length]);
        // Keep the producer open: a bounded parser must reject before EOF.
        let deadline = Instant::now() + Duration::from_secs(2);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let stopped = child.try_wait().unwrap().is_some();
        if !stopped {
            child.kill().unwrap();
        }
        drop(stdin);
        let out = child.wait_with_output().unwrap();
        assert!(
            stopped,
            "{format} waited for EOF instead of rejecting oversized input"
        );
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("limit"));
    }
}

#[test]
fn duckdb_memory_limit_rejects_invalid_and_irrelevant_values() {
    for size in [
        "0MiB",
        "-1MiB",
        "1.5MiB",
        "unlimited",
        "80%",
        "999999999999999999999GiB",
        "1MiB'; SELECT 42;--",
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
            .args([
                "export",
                "--format",
                "parquet",
                "--output",
                "unused.parquet",
                "--duckdb-memory-limit",
                size,
                "-",
            ])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("--duckdb-memory-limit"));
    }
    let out = Command::new(env!("CARGO_BIN_EXE_ls-lr-analyzer"))
        .args(["export", "--duckdb-memory-limit", "64MiB", "-"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("requires Parquet"));
}
