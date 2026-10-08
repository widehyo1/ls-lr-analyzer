#!/usr/bin/env python3
"""Compare installed/target binaries using a record-aligned find snapshot prefix."""
import argparse
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import tempfile


def prefix(source, destination, fraction):
    target = source.stat().st_size // fraction
    fields = []
    carry = b""
    written = records = 0
    with source.open("rb") as reader, destination.open("wb") as writer:
        while chunk := reader.read(1024 * 1024):
            parts = (carry + chunk).split(b"\0")
            carry = parts.pop()
            for field in parts:
                fields.append(field)
                if len(fields) == 10:
                    record = b"\0".join(fields) + b"\0"
                    writer.write(record)
                    written += len(record)
                    records += 1
                    fields.clear()
                    if written >= target:
                        return {"bytes": written, "records": records, "source_bytes": source.stat().st_size}
    raise RuntimeError("Input ended before a complete prefix could be collected")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=pathlib.Path)
    parser.add_argument("--baseline", default=shutil.which("ls-lr-analyzer"))
    parser.add_argument("--target", default="target/release/ls-lr-analyzer")
    parser.add_argument("--fraction", type=int, default=10)
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    if args.fraction <= 0 or not args.baseline:
        parser.error("A positive fraction and installed baseline binary are required")
    output = (args.output or pathlib.Path(tempfile.mkdtemp(prefix="ls-lr-comparison-"))).resolve()
    output.mkdir(parents=True, exist_ok=True)
    snapshot = output / "subset.find"
    if snapshot.exists():
        parser.error("Output directory already contains subset.find")
    data = prefix(args.source.resolve(strict=True), snapshot, args.fraction)
    print(json.dumps({"artifacts": str(output), "subset": data}), flush=True)
    result = {"source": str(args.source.resolve()), "subset": data, "runs": []}
    binaries = {"baseline": pathlib.Path(args.baseline).resolve(strict=True), "target": pathlib.Path(args.target).resolve(strict=True)}
    result["binaries"] = {name: str(binary) for name, binary in binaries.items()}
    result["versions"] = {name: subprocess.check_output([str(binary), "--version"], text=True).strip() for name, binary in binaries.items()}
    for mode in ("capacity", "comparison"):
        for name, binary in binaries.items():
            stem = f"{mode}-{name}"
            report = output / f"{stem}.txt"
            metrics = output / f"{stem}.time.json"
            stderr = output / f"{stem}.stderr"
            argv = [str(binary), "--input-format", "find", str(snapshot)]
            if mode == "comparison":
                argv += [str(snapshot), "--depth", "2"]
            # A private disk-backed temporary directory keeps the two runs isolated.
            # Run serially so one process's I/O does not distort the other's timing.
            with tempfile.TemporaryDirectory(prefix=stem + "-", dir=output) as scratch:
                env = dict(os.environ, TMPDIR=scratch)
                with report.open("wb") as stdout, stderr.open("wb") as errors:
                    completed = subprocess.run([
                        "/usr/bin/time", "-f", '{"rss_kib":%M,"elapsed_s":%e,"user_s":%U,"system_s":%S}',
                        "-o", str(metrics), *argv,
                    ], stdout=stdout, stderr=errors, env=env)
                if completed.returncode:
                    raise RuntimeError(f"{stem} failed; see {stderr}")
            measurement = json.loads(metrics.read_text())
            measurement.update(mode=mode, binary=name, report_sha256=hashlib.sha256(report.read_bytes()).hexdigest())
            result["runs"].append(measurement)
            print(json.dumps(measurement), flush=True)
        equal = (output / f"{mode}-baseline.txt").read_bytes() == (output / f"{mode}-target.txt").read_bytes()
        result[f"{mode}_reports_equal"] = equal
        if not equal:
            (output / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
            raise RuntimeError(f"{mode} reports differ; inspect artifacts in {output}")
    (output / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"Reports match. Summary: {output / 'summary.json'}", flush=True)


if __name__ == "__main__":
    main()
