# SQLite history storage

Schema 14 (0.3.1) keeps all existing scan points, details, coverage flags and
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

## Shared history paths and compressed detail

`history_paths(id, parent, fragment)` stores exact path components shared across
scans and between details, directory summaries and extension summaries. ID 0 is
the empty path. Other rows refer to a smaller parent ID; concatenating parent
text and fragment reconstructs the exact original string. Separators remain in
fragments, and the unique `(parent, fragment)` index preserves case distinctions.
IDs describe historical strings, independently of current file identities and
renames. Nodes and the incremental scan checkpoint retain their existing format.

The newest scan on each volume keeps rows in `changes(seq, scan, old_path_id,
new_path_id, logical_delta, allocated_delta, kind)`. Earlier rows move to
`change_archives(scan, part, entries, data)`. Each part contains at most 256
rows: a UTF-8 JSON array compressed with zlib:

```
[sequence, old_path_id, new_path_id, logical_delta, allocated_delta, kind]
```

`sequence` preserves the source rowid and stable pagination. A part is decoded
and compared with its input before live rows are removed. Each scan's compression
commits atomically; cancellation rolls back the active scan and supports retry.
Directory/type tables use `path_id` references to the same path table.

Every application connection registers `volumetrail_inflate(blob)` and temporary
`change_history` / `change_counts` views. The schema-14 detail view exposes numeric
path references. Public queries filter and paginate before resolving paths.
Directory filters traverse the shared hierarchy using its parent index, with the
same case-insensitive matching as earlier versions. Independent SQL tools must
decode zlib plus JSON and resolve IDs through `history_paths`; reading `changes`
alone omits compressed history.

Read-only access to earlier schemas still supports literal path rows and both
legacy compressed representations: plain row arrays and block-local path IDs in
`{"rows": [...]}`. Writable access converts them together in one transaction.
Archive entry counts and compression round trips are checked during conversion;
malformed data aborts the transaction. Foreign keys cover live and summary path
references, and invalid compressed references produce query errors.

Path caches belong to individual queries or write operations. A rolled-back
scan cannot leave cached IDs for later writes. Explicit detail/scan deletion
marks live, summary and compressed references, retains their ancestors, then
releases unused paths in the same transaction. Automatic maintenance retains
all scan points and details.

Schema 13 adds a nullable unique `scans.task_id` for durable scan completion.
It detects a successfully published scan whose work file survived a process
exit. Existing rows retain their original values and a null task ID.

## Migration and verification

Schema-14 migration reserves the writer before taking a consistent SQLite
backup named `history-before-v14-<timestamp>-<process>.db` beside the database.
The backup is completed before schema/data conversion begins. The conversion
uses a FULL-synchronous transaction and changes the schema version only with
the completed data. A process exit recovers the original tables and permits
migration to restart; an already migrated database is left as-is.

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
.\build.ps1 test -CargoArgs '--lib','--test','store','--test','storage_compaction','--test','path_sharing'
```

These cover legacy conversion, compressed queries and pagination, new scans,
explicit deletion and path reclamation, cancellation, mixed-format migration,
backup preservation, abrupt migration exit, malformed archives and scan-launch status.
