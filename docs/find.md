# Saved find snapshots

The analyzer reads saved snapshots only. It never runs find or examines paths
on the analysis machine. GNU find is needed only on the collection machine;
local analysis works without find, even with an empty PATH. No Cargo feature is
required for find input.

## Collect on the source machine

Use GNU-compatible find and this exact ten-field NUL-delimited format:

```bash
# Capture inside the selected root; -P does not follow symbolic links.
(
  cd /data || exit
  LC_ALL=C find -P . -xdev -name '.*' ! -name . -prune -o \
    -printf '%P\0%y\0%s\0%b\0%M\0%n\0%U\0%G\0%T@\0%l\0'
) > /tmp/snapshot.find 2> /tmp/snapshot.find.stderr
# Check the exit status immediately. Discard the snapshot if nonzero,
# or if snapshot.find.stderr is nonempty.
```

The files must be outside the collected tree. Transfer the snapshot only after
successful collection. A mounted binary can be invoked as `/tools/find` in this
command; there is no `--find-bin` analyzer option. Remove `-xdev` to descend
across devices. To include hidden entries, remove the prune expression and use
`find -P . -xdev -printf '...'` with the same format.

## Analyze elsewhere

```bash
ls-lr-analyzer report --input-format find snapshot.find
ls-lr-analyzer report --input-format find before.find after.find --days 3
ls-lr-analyzer export --input-format find snapshot.find --output snapshot.tsv
ls-lr-analyzer export --input-format find snapshot.find \
  --format parquet --output snapshot.parquet
ls-lr-analyzer report --input-format tsv before.tsv after.tsv
```

`--source find` is an alias for selecting raw find snapshots. Neither interface
accepts live directories. Existing TSV/SQLite/Parquet exports retain their own
input format and need no find options.

## Format and limitations

- Each record has ten NUL-terminated fields: relative path (`%P`), type (`%y`),
  logical size (`%s`), allocated 512-byte blocks (`%b`), permissions (`%M`),
  link count (`%n`), numeric UID/GID (`%U`, `%G`), epoch timestamp (`%T@`),
  and symlink target (`%l`). The first record is the root directory with an
  empty relative path. Directory records, including empty directories, are
  required. Plain find path listings are not supported.
- NUL delimiters preserve whitespace, newlines and backslashes. The shared
  export schema is UTF-8; non-UTF-8 fields are rejected. Duplicate paths,
  invalid fields, missing directory records and truncated records are rejected.
  A stream truncated exactly at a record boundary cannot always be detected:
  validate collection status and stderr on the source machine.
- Direct-child allocated totals use `%b * 512`, including directory and link
  blocks but not the root's own blocks. Exports use `total_bytes`, independent
  of report `--block-size`. Timestamp columns contain `epoch`, exact `%T@`,
  and `find`; ownership is numeric. Compare the same source format and scope.
- `-xdev` stops descent across device IDs but retains boundary directories;
  same-device bind mounts may still be traversed. The hidden-entry policy is
  decided during collection, not analysis. Overlay merged report filtering
  remains controlled by `--include-merged`.
- This is not an atomic snapshot or a content/ACL integrity check. Hard links,
  shared extents, sparse files and deleted open files limit physical-usage
  conclusions. Symlink-target changes are retained but not analyzed.
- Normalization groups metadata in memory and stages normalized TSV in the
  system temporary directory. Plan memory and temporary space for large files.
