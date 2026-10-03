# Changelog

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
