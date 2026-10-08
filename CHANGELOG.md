# Changelog

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
