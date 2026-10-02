# ls-lr-analyzer

A command-line tool for finding directory growth in saved GNU `ls -lR` listings.
Analyze one snapshot for capacity, or compare two snapshots for changes in file
sizes, file counts, timestamps, and directory block totals. Reports are plain
text for people, not machine-readable output.

## Quickstart

Install from this checkout, then compare two listings:

```bash
cargo install ls-lr-analyzer
ls-lr-analyzer before.txt after.txt
```

After the first crates.io release, install with `cargo install ls-lr-analyzer`.
Building requires Rust 1.97 or newer.

## Daily use

```bash
# Inspect the capacity distribution of one snapshot.
ls-lr-analyzer snapshot.txt

# Compare snapshots with deeper grouping and shorter rankings.
ls-lr-analyzer before.txt after.txt --depth 2 --top 5

# Show average net growth over a known three-day interval.
ls-lr-analyzer before.txt after.txt --days 3

# Save the report or read it in a pager.
ls-lr-analyzer before.txt after.txt > report.txt
ls-lr-analyzer before.txt after.txt | less

# Show usage and options.
ls-lr-analyzer --help
```

Capture each listing with consistent units, locale, and filename quoting:

```bash
LC_ALL=C ls -lR --block-size=1K --quoting-style=literal /data > before.txt
# Repeat after the observation interval, writing to after.txt.
LC_ALL=C ls -lR --block-size=1K --quoting-style=literal /data > after.txt
```

Use the original listings, not a `.diff` file. Capture the same complete logical
root both times; changes during collection can affect the results.

## Options

```text
ls-lr-analyzer [OPTIONS] [BEFORE] AFTER
```

| Option | Effect |
| --- | --- |
| `--depth N` | Group paths at depth N, including descendants. Default: 1. |
| `--top N` | Limit each ranking to N entries. Default: 10. |
| `--days N` | Calculate average net growth over a user-supplied interval. Comparison only. |
| `--block-size N` | Bytes per directory `ls total` unit. Default: 1024. |
| `--include-merged` | Include container overlay merged views, excluded by default. |
| `--` | Treat all remaining arguments as input file paths. |
| `-h`, `--help` | Print usage. |

Numeric values must be positive. `--days` also accepts finite fractional values.

## Reading the report

- The summary separates logical regular-file size from directory block totals.
- Size changes are broken down into new files, existing-file growth, removed
  files, and existing-file shrinkage. This exposes turnover such as log rotation.
- Path groups include descendants, but each file belongs to exactly one group.
  Root-level files belong to `.`. Comparison groups rank by absolute net change;
  single-snapshot groups rank by current size.
- Directory rankings count direct files only, preventing parent and child
  directories from reporting the same growth twice.
- File-count changes highlight many small additions that size rankings may miss.
- Largest file increases and decreases are separate from largest current files.
- Timestamp, attribute, and block changes can indicate activity without growth.
- Warnings and excluded overlay totals appear at the end of the report.

The tool reports paths and measurements rather than guessing service categories.
`--days` describes the supplied interval; it is not a long-term forecast.

## Compatibility and limitations

- Supports the ordinary GNU `ls -lR` format, English and Korean date/total
  markers, numeric owners/groups (`-n`), and literal filenames containing spaces.
  Tool-generated messages are in English; input paths and diagnostic examples
  retain their original text.
- Root header names may differ, such as `/data` and `.`, and directory ordering
  may differ. Multiple independent roots in one input are not supported.
- Human-readable sizes (`ls -lhR`), `--full-time`, and filenames containing
  newlines are not supported. Keep date formats consistent between snapshots.
- Only regular files contribute to logical sizes. Symlinks are not followed.
  Directory block totals can include directories, links, and special files.
- The listing does not establish the block unit; match `--block-size` to the
  collection command. Size and displayed timestamp are metadata, not content
  checksums; unchanged values can hide content changes.
- Parse errors, `ls` errors, and missing totals produce warnings. Incomplete
  collection can make inaccessible files appear removed.
- Hard-link deduplication, shared extents, duplicate mounts, and deleted open
  files cannot be determined. Results are not equivalent to `df` or deduplicated
  `du`, and file counts are not exact inode counts.
- Only `containers/storage/overlay/<ID>/merged` and its descendants are excluded
  by default. Including these unified views can double-count storage layers.
- Analysis reads listing text, never the underlying file contents. Comparison
  keeps before-file metadata in memory; usage scales with file count and name
  length. A single snapshot is read directory by directory.

## Project

[Source](https://github.com/widehyo1/ls-lr-analyzer) · [MIT license](LICENSE)
