# SQLite history storage

Schema 13 (0.3.1) keeps all existing scan points, details, coverage flags and
directory/type aggregates. Automatic maintenance compresses history. Explicit
user actions can still clear selected details or delete scan points.

## Current index

`nodes` retains its indexed columns. Its JSON `payload` stores `modified`,
`attributes`, `links` and an optional `primary_link` flag. Readers combine these
with the indexed columns to reconstruct the complete `Entry`, including unsigned
file identities. When the first link matches `parent` and `name`, `primary_link`
represents that link and readers prepend it to the stored remaining links.
Empty link lists and additional hard links retain their original order.

Opening a writable schema-11 database migrates its payloads in one transaction.
Read-only access also accepts the legacy full-entry JSON format.

## Compressed detail

The newest scan on each volume keeps its rows in `changes`. Earlier rows move
to `change_archives(scan, part, entries, data)`. Each part contains at most 256
rows: a UTF-8 JSON array compressed with zlib. Each row has these fields:

```
[sequence, old_path, new_path, logical_delta, allocated_delta, kind]
```

`sequence` is the source rowid used for stable ordering. A part is decompressed
and compared with its source JSON before its original rows are removed. Each
scan is committed atomically. Cancellation rolls back the active scan's
compression; already compressed scans remain available and retry is supported.

Parts also support `{"rows": [...]}` with shared path IDs. In this representation
the first occurrence of each path is a string that assigns the next zero-based
ID; later occurrences use that integer ID in the old/new path fields. IDs are
local to the part. The encoder compares the compressed size of both formats and
keeps the smaller one. This bounds dictionary lifetime to its detail part and
allows normal scan/detail deletion to reclaim it together with the rows.
`volumetrail_inflate` expands this representation to the plain row array, keeping
the SQL view and query semantics consistent across both formats.

Every application connection registers `volumetrail_inflate(blob)` and temporary
`change_history` / `change_counts` views. All detail queries read these shared
views. The views exist only in the connection. Independent SQL tools can decode
zlib plus JSON and combine the decoded rows with `changes`; scanning `changes`
alone omits compressed history. Directory/type summaries retain their tables.

Foreign keys cascade archive deletion when a user deletes a scan. Explicit
detail cleanup removes both live rows and compressed parts.

Schema 13 adds a nullable unique `scans.task_id` for durable scan completion.
It detects a successfully published scan whose work file survived a process
exit. Existing rows retain their original values and a null task ID.

## Migration and verification

Stop the application and workers before replacing a database. Keep a consistent
SQLite backup of the original. Upgrade and compress a separate copy with
`VolumeTrail-cli.exe maintain C` beside that copy's `data/` directory. To reclaim
partly filled index pages, use SQLite `VACUUM INTO` with an explicit destination
on a drive with sufficient free space while the copy is offline.

Before installation, compare all scan/summary rows, reconstructed index entries
and complete detail rows against the original, then run `integrity_check` and
`foreign_key_check`. Keep the original database and matching binaries together
for rollback: earlier binaries expect the earlier payload and detail format.

Regression tests:

```powershell
.\build.ps1 test -CargoArgs '--lib','--test','store','--test','storage_compaction'
```

These cover legacy conversion, compressed queries and pagination, new scans,
explicit deletion, cancellation, malformed archives and scan-launch status.
