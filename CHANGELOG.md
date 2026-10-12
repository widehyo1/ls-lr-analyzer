# Changelog

## 0.1.7

- Stream database/Parquet reads to disk and restore row-index order with bounded external sorting, removing whole-snapshot SQL sorting and CLI result materialization.
- Check delivered row counts against source counts; reject incomplete client output even when the client exits successfully.
- Support Parquet-to-SQLite export; bound SQLite read/export heap, page cache, and child address space, and index new snapshots by row number.
- Allow `--duckdb-memory-limit SIZE` for Parquet-to-SQLite export input; retain the default 64 MiB DuckDB budget and engine-default CSV memory settings.
- Test shuffled and signed row indices, incomplete successful client output, SQLite heap exhaustion, read-path address-space limits, and failure cleanup.
- Verify full Parquet report, Parquet-to-SQLite export, and SQLite report with 11,272,039 actual rows; confirm metadata hashes and report results match.

## 0.1.6

- Reject oversized stdin/file fields and lines before unbounded allocation; bound temporary sort records and report ranking counts.
- Add `--duckdb-memory-limit SIZE` for Parquet export and DuckDB/Parquet report input (default: 64 MiB).
- Export Parquet through bounded DuckDB batches while retaining engine-default CSV buffers.
- Join batches into one Parquet file with relocated offsets and a disk-spooled footer; preserve all cells and row order.
- Apply a Linux address-space ceiling to every DuckDB child, including capability checks, and refuse unguarded execution on other platforms.
- Preserve atomic publication and remove partial batches on ordinary failures.
- Require Rust 1.99 and test oversized open stdin, multi-batch output, budget propagation, and failure cleanup.

## 0.1.5

- Accept `-` for streamed ls, find, and TSV report input with bounded memory.
- Support stdin ls/find input for TSV and enabled SQLite/Parquet exports.
- Allow stdin on either side of a comparison; reject two stdin inputs.
- Document explicit stdin selection and file-only database/Parquet report input.
- Test file/stdin equivalence, input validation, overwrite protection, and cleanup.

## 0.1.4

- Replace full find path/metadata collections with bounded-memory external sorting.
- Stream report metadata without retaining all files in a directory or the before snapshot.
- Merge-join normalized relative paths for exact additions, removals, and metadata changes.
- Accumulate totals and retain bounded top-K rankings; reduce path groups on disk.
- Preserve export schemas, entry order, duplicate/parent validation, and report tie ordering.
- Bound backend diagnostics and configure DuckDB working memory and disk spill.
- Add multi-run comparison, grouping, validation, and temporary cleanup regression tests.
- Add a real-snapshot baseline/target RSS and output comparison script.

## 0.1.3

- Read saved GNU find snapshots; document source-machine collection commands.
- Reuse all export/report formats with precise timestamps and byte block totals.
- Test offline find snapshot input and document its format and limitations.
- Announce file export start and completion on stderr, including elapsed time.
- Remove DuckDB file export; bulk-load Parquet data using in-memory DuckDB.

## 0.1.2

- Add optional SQLite, DuckDB, and Parquet exports with runtime capability checks.
- Separate `export` and `report`; report from all exported formats.
- Add Cargo features and configurable CLI binary paths for container mounts.
- Add `-V` / `--version`, format fixtures, and CLI/report equivalence tests.
- Separate CLI, input, export, and report code; document installation options.

## 0.1.1

- Add `--format tsv` to export listing metadata as a portable intermediate artifact.
