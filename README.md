# ls-lr-analyzer

A command-line tool for finding directory growth in saved GNU `ls -lR` listings.
Analyze one snapshot for capacity, or compare two snapshots for changes in file
sizes, file counts, timestamps, and directory block totals. Reports are plain
text for people by default. Export a snapshot as TSV for a portable intermediate
artifact that can be read without database or columnar-format tools.

## Quickstart

Install from crates.io, then compare two listings:

```bash
cargo install ls-lr-analyzer
ls-lr-analyzer before.txt after.txt
```

### Install options

The default build provides text reports and TSV export, with no external runtime
tools and no third-party crates. Enable optional file formats with Cargo features:

```bash
# Enable selected formats.
cargo install ls-lr-analyzer --features sqlite
cargo install ls-lr-analyzer --features duckdb,parquet

# Enable all optional formats.
cargo install ls-lr-analyzer --all-features

```

| Feature | Output format | Runtime executable |
| --- | --- | --- |
| `sqlite` | SQLite database | `sqlite3` |
| `duckdb` | Existing DuckDB database report input only | `duckdb` |
| `parquet` | Parquet file | `duckdb` |

Features are independent and add no crate dependencies. Enabling `parquet` does
not require enabling `duckdb`. Installation does not require the external
executables; they are checked when reading or exporting that format. Each
`sqlite` and `parquet` enable export and report input; `duckdb` enables report
input only for existing database snapshots.

## Build from source

Building requires Rust 1.97 or newer. To build and install from a checkout:

```bash
git clone https://github.com/widehyo1/ls-lr-analyzer.git
cd ls-lr-analyzer
cargo build --release
cargo install --path .

# Optionally enable selected formats or all formats.
cargo install --path . --features sqlite,duckdb,parquet
cargo install --path . --all-features
```

## Daily use

### Saved find snapshots

```bash
# On the collection machine; save outside the selected directory.
cd /path/to/target
LC_ALL=C find -P . -xdev -name '.*' ! -name . -prune -o \
  -printf '%P\0%y\0%s\0%b\0%M\0%n\0%U\0%G\0%T@\0%l\0' \
  > /tmp/snapshot.find 2> /tmp/snapshot.find.stderr
# Verify exit status is zero and stderr is empty before transferring the file.

# On the analysis machine, no find binary or original filesystem is needed.
ls-lr-analyzer report --input-format find snapshot.find
ls-lr-analyzer report --input-format find before.find after.find
ls-lr-analyzer export --input-format find snapshot.find --output snapshot.tsv
ls-lr-analyzer report --input-format tsv before.tsv after.tsv
```

The default remains saved `ls -lR` input. Find input also reads saved files only;
the analyzer never launches find. The collection command excludes hidden entries
and does not follow links. `-xdev` belongs to collection, not analysis. See
[find snapshot format and cautions](docs/find.md) for hidden-entry collection
and validation details. No additional Cargo feature is needed for find input.

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

# Save the listing metadata as a machine-readable intermediate artifact.
ls-lr-analyzer export snapshot.txt > snapshot.tsv

# Analyze or compare saved intermediate artifacts.
ls-lr-analyzer report snapshot.tsv --input-format tsv
ls-lr-analyzer report before.tsv after.tsv --input-format tsv --depth 2 --top 5
```

Capture each listing with consistent units, locale, and filename quoting:

```bash
LC_ALL=C ls -lkR --quoting-style=literal /data > before.txt
# Repeat after the observation interval, writing to after.txt.
LC_ALL=C ls -lkR --quoting-style=literal /data > after.txt
```

Use complete snapshots, not a `.diff` file. Capture the same complete logical
root both times; changes during collection can affect the results.

## Options

```text
ls-lr-analyzer [OPTIONS] [BEFORE] AFTER
ls-lr-analyzer export [OPTIONS] SNAPSHOT
ls-lr-analyzer report [OPTIONS] [BEFORE] AFTER
```

`export` saves metadata from one original listing; its default format is TSV.
`report` reads one snapshot for capacity or two for comparison, producing the
same human-readable report for every input format. Its default input format is
`ls`. Both comparison inputs use the selected format; extensions are not used to
infer formats. The original interface without a subcommand remains supported,
including the earlier `--format tsv` export syntax.

| Option | Effect |
| --- | --- |
| `--depth N` | Report: group paths at depth N, including descendants. Default: 1. |
| `--top N` | Report: limit each ranking to N entries. Default: 10. |
| `--days N` | Report: average net growth over a supplied interval. Comparison only. |
| `--block-size N` | Report: bytes per directory `ls total` unit. Default: 1024. |
| `--include-merged` | Report: include container overlay merged views, excluded by default. |
| `--format FORMAT` | Export: `tsv` (default), `sqlite`, or `parquet`. |
| `--input-format FORMAT` | `ls` (default), `find`, `tsv`, `sqlite`, `duckdb`, or `parquet`; export accepts `ls`/`find`. |
| `--output FILE` | Export destination; TSV defaults to stdout, other formats require a file. Must not already exist. |
| `--sqlite3-bin PATH` | SQLite CLI executable; default: `sqlite3` found on `PATH`. |
| `--duckdb-bin PATH` | DuckDB CLI executable for DuckDB and Parquet; default: `duckdb` found on `PATH`. |
| `--source SOURCE` | `ls` (default), or `find` to read saved raw find snapshots. |
| `--` | Treat all remaining arguments as input file paths. |
| `-h`, `--help` | Print usage. |
| `-V`, `--version` | Print the package version. |

Numeric values must be positive. `--days` also accepts finite fractional values.

Exports with `--output` print `started` immediately and `finished` with elapsed
seconds after successful publication, both on stderr. Failures print an error,
not a success message. TSV exports to stdout have no progress messages.

## TSV intermediate artifact

`export --format tsv` takes one input and streams UTF-8 TSV to stdout, with a header
and exactly 12 columns per row:

```text
record directory permissions links owner group size_or_device month day time_or_year name total
```

The actual column separators are tabs. `record` is `directory` for a listing
header (including empty directories), `total` for its block total (`total_bytes`
for find's byte totals), or `entry`
for a file, directory, symlink, or special file. `directory` preserves the
original listing header path. Entry rows retain the eight metadata fields and
filename; permissions identify the entry type. Symlink `name` includes the
displayed ` -> target` suffix, without attempting to split ambiguous filenames.
For devices, `size_or_device` contains `major,minor`; otherwise it contains
the displayed size. `total` retains the original number in the listing's block
units; no conversion or human-readable size formatting is applied. Unused
cells are empty.

Every cell escapes backslash as `\\`, tab as `\t`, carriage return as `\r`, and
newline as `\n`. Decode these escapes in one pass when reading values. Spaces,
Unicode, and quotes remain literal. This guarantees one physical line per row
and an unambiguous tab separator, even for filenames containing tabs. The source
listing must still use the supported ordinary `ls -lR` format; filenames with
newlines cannot be recovered from that format.

Export includes all entries in source order, including overlay merged views.
Report options (`--depth`, `--top`, `--block-size`, `--include-merged`) are applied
when producing reports, and are rejected by the explicit `export` command.
`ls_error` and `unparsed` rows preserve problematic input lines in `name`, with
a warning on stderr. `report --input-format tsv` decodes escaped cells and reads
these rows without external tools.

## Database and Parquet exports

With the corresponding features enabled:

```bash
ls-lr-analyzer export snapshot.txt --format sqlite --output snapshot.sqlite
ls-lr-analyzer export snapshot.txt --format parquet --output snapshot.parquet

ls-lr-analyzer report snapshot.sqlite --input-format sqlite
ls-lr-analyzer report before.duckdb after.duckdb --input-format duckdb
ls-lr-analyzer report before.parquet after.parquet --input-format parquet --days 3

# Use binaries supplied through a container volume mount.
ls-lr-analyzer export snapshot.txt --format sqlite --sqlite3-bin /tools/sqlite3 \
  --output snapshot.sqlite
ls-lr-analyzer export snapshot.txt --format parquet --duckdb-bin /tools/duckdb \
  --output snapshot.parquet
ls-lr-analyzer report snapshot.parquet --input-format parquet \
  --duckdb-bin /tools/duckdb
```

Executable paths are passed directly to the operating system without a shell.
A bare executable name is searched on `PATH`; an explicit path selects that
binary. Mounted binaries must be executable and compatible with the container's
OS, architecture, and runtime libraries.

SQLite exports first execute `SELECT 42` and check the returned value.
Parquet exports use the selected DuckDB binary to write a temporary Parquet file
and read back its value. Missing executables, failed capability checks, and
backend errors produce an error without publishing a database or Parquet output.
CLI startup configuration is replaced with an empty initialization file.

Database files contain a `snapshot` table. Parquet contains the same columns:
the 12 TSV columns plus a 1-based `row_index` integer preserving source order.
The metadata columns are all text, including displayed sizes and totals, to
preserve original values without type inference or signed-integer overflow.
Unused values are empty strings. Tabs and backslashes are stored as their actual
characters; TSV escape sequences are not stored in these formats. All export
formats preserve directory headers, totals, special entries, and diagnostic rows.

```sql
SELECT directory, name, size_or_device
FROM snapshot
WHERE record = 'entry' AND permissions LIKE '-%'
ORDER BY row_index;
```

For Parquet, replace `snapshot` with `read_parquet('snapshot.parquet')` in DuckDB.

DuckDB database export is no longer offered. Existing DuckDB snapshots remain
readable with the `duckdb` feature. Parquet export bulk-loads a temporary
hex-encoded TSV in DuckDB's `:memory:` database, then writes Parquet directly;
it does not create a database file or issue per-row INSERT statements. The
temporary TSV still requires disk space, and DuckDB may spill to disk under
memory pressure.
SQLite export uses temporary SQL and a staged database; Parquet uses a staged
hex-encoded TSV and Parquet file beside the destination. Allow disk space for
these intermediate files. The
destination directory must exist and support hard links: the completed artifact
is published without overwriting an existing file. Temporary files are cleaned
up after success or ordinary errors.

TSV file output uses the same staging and publication rules. Redirecting TSV
stdout uses the shell's normal redirection behavior.

No new third-party crates are used for these formats.

Reports open database inputs read-only. Database and Parquet rows are transferred
through a temporary file in the system temporary directory; metadata is then
processed directory by directory. The same analysis and report renderer are used
for original listings and all intermediate formats. Export preserves block
totals in their original units, so supply the same `--block-size` when reporting
ls source or exported data; find's `total_bytes` records need no unit option.
CLI file reports contain the supplied artifact
paths in their input labels.

### Test fixtures

`tests/fixtures/` contains `before` and `after` snapshots in `.txt`, `.tsv`,
`.sqlite`, `.duckdb`, and `.parquet` forms. They were generated using SQLite
3.37.2 and DuckDB 1.4.0. To regenerate one artifact, move the existing file aside
and run:

```bash
cargo run --all-features -- export tests/fixtures/before.txt \
  --format parquet --output tests/fixtures/before.parquet
```

`cargo test --all-features` compares capacity and comparison reports across the
fixture formats, covering the main report option combinations. Tests also cover
CLI defaults, invalid options, version/help, escaped metadata, explicit binary
paths without `PATH`, missing tools, backend errors, and overwrite protection.
External-format tests skip when their binaries are absent; install both tools
to exercise every backend.

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
