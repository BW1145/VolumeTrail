# Scan traversal and recovery

Directory-move statistics traverse `nodes_parent(volume,parent)` from each
changed parent. Explicit parent-first joins keep both the recursive directory
step and the file lookup on that index. `UNION` deduplicates overlapping changed
subtrees, and staged file identities are excluded from directory compensation.

## Durable work

Each volume has one application-owned `data/scan-work-<drive>.db`. It contains:

- `meta`: task ID, format, baseline/checkpoint, volume/root/journal identities,
  MFT cursor, journal cursor, counters, pending reads and warnings.
- `entries`: collected upserts and confirmed deletions keyed by complete NTFS
  file identity, including its MFT sequence.
- `events`: deduplicated journal identities, deletion flags and read completion.
- `statistics`: old/new-path compensation batches, each storing its signed
  phase, part, cursor, completion flag and derived path/type contributions.

Work uses SQLite DELETE journaling and FULL synchronous mode. Each collection
buffer commits its entries, pending reads and Windows-provided cursor in one
transaction. A partially saved buffer is replayed. MFT cursors use the next
file-reference value returned by `FSCTL_ENUM_USN_DATA`; journal cursors use the
next USN returned by `FSCTL_READ_USN_JOURNAL`.

Resuming validates format, original checkpoint and scan ID, cleanup-verification
context, volume/root identity, journal identity and retained cursor range. It
reads the journal tail before continuing file reads. Changed inputs invalidate
cached statistics in the same work transaction, so results computed from older
inputs cannot be reused. Unchanged inputs keep their cached batches.

Statistics checkpoint every 512 file identities. Their cursor and derived data
share one row. A retry folds each saved part once, then continues after its last
identity. Parent-first traversal retains cancellation and load yielding.

## Complete publication

The published database keeps its last complete index and history throughout
collection. Collected inputs are imported into its existing staging transaction.
Index changes, folder/type summaries, the new scan point, successful checkpoint
and unique task ID commit atomically. This transaction uses FULL synchronous
mode before durable work is reclaimed. Interruption
replays that transaction; durable inputs and completed statistics remain usable.
An interrupted final transaction cannot expose a partial published scan.

After success the work database is removed. If the process exits after commit
and before removal, the task ID recognizes the existing scan on the next run.
The work file is then reclaimed without publishing the same task again.

The UI offers pause-and-save, continue, and discard-unfinished-work. CLI `stop C`
preserves saved progress; `discard-scan C` removes only that volume's unfinished
work. Manual scans and login scans share this lifecycle and the existing global
and per-volume locks. Invalidated work is re-collected with a specific status
warning; complete published history remains available.

## Verification

Unit and integration tests cover index-preserving cancellation, cursor/entry
atomicity, journal/base validation, additional tail events, statistics invalidation,
interruptions in both compensation phases, process exit with an open transaction,
idempotent publication, and exact result comparison against continuous execution.
The bundled SQLite traversal tests compare complete results and both parent-index
query-plan steps. Production-shaped benchmarks run only on isolated copies.

Windows cursor references:
[MFT enumeration](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_enum_usn_data),
[journal reads](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_read_usn_journal).
