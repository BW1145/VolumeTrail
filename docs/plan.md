# VolumeTrail v0.3.1 implementation

## Product

Portable Rust + egui app with a separate scan process. The scan starts at login
when enabled, processes the configured NTFS volumes sequentially, then exits.
SQLite and configuration live beside the executable in `data/`. v0.3.1 is
[published](https://github.com/BW1145/VolumeTrail/releases/tag/v0.3.1);
installation and data-preserving upgrades are in the [README](../README.md).

## Delivered behavior

- The compact overview shares a query service with `query overview`. It shows
  current-index usage, the growth interval, leading increases/decreases and the
  latest cleanup result; cleanup verification points stay linked to their scan.
- User-defined cleanup rules bind a volume, directory, filename patterns,
  recursion and modification age. Saving or editing always pauses the rule.
  A 15-minute preview precedes explicit one-time deletion or separate automatic
  enablement. Deletion is permanent, with no recycle bin or file-content backup.
- Enabled rules run after a successful login scan on their configured volume,
  at most once per rule per local day. Manual scans only scan. The existing
  login task lifecycle is reused; enabling a rule does not install that task.
- Cleanup records retain rule snapshots, item outcomes and allocation estimates;
  execution and index verification are independent states. Cancellation stops
  subsequent files and never restores deleted ones. Interrupted items are not
  replayed. See [automation.md](automation.md) for permissions and retention.
- `capabilities --json` discovers the structured query/rule/preview/execution
  interface. Plan IDs and acknowledgement flags do not establish user authorization.

- Scan summaries retain sample time, completion time, disk capacity/free space,
  indexed bytes and file count. The chart can show 7, 30, 365 days or all points.
- Older per-file details use lossless compressed chunks; the latest scan per
  volume stays directly queryable. Scan summaries, details, directory and type
  aggregates are retained automatically. The current index stores each field once.
  Storage totals distinguish index, history and migration backups. The UI can
  explicitly clear selected details or delete selected scan points.
- Deleting an intermediate point merges its directory interval into the next
  retained interval. Deleting the latest point carries its interval into the
  next scan. Comparison coverage is shown when aggregates or details are absent.
- Directory growth includes moved content. File identity is counted once, and
  a moved directory with changed children is not counted twice.
- The changes page supports directory drilldown, extension totals and paged
  file details. Drilldown retains the directory total and direct-level changes
  even when there are no child directories. The current folder page includes largest directories and files.
- `VolumeTrail-cli.exe query` reads summaries, growth, directories and largest
  items directly from the database with text or JSON output.
- CPU and relevant logical-disk load are sampled every two seconds during
  scanning and commit. The scan pauses at high load and resumes after two
  quieter samples. It keeps background process priority and checks cancellation.
- Journal events are merged by the complete file reference before reading the
  changed NTFS entries. Deletions remain separate from reused MFT slot identities.
- Scan summaries save timings and counters plus CPU time and worker peak memory
  obtained at scan boundaries, with no additional performance sampling loop.
- One global lock serializes scans and history writes across all volumes.
  UI comparisons and rankings use separate read-only connections.
- Opening an older database migrates its schema. The pre-v2 and schema-14 migrations create
  consistent history backups. Schema 14 converts all history path references
  in one durable transaction; an interrupted conversion can restart. Preserve
  a separate full `data/` copy before upgrading. Legacy changes are
  aggregated to the extent their saved deltas permit.

## Verification

`build.ps1 test -CargoArgs '--locked','--all-targets'` covers accounting, movement, history deletion/carry, retention,
current-index queries, cancellation rollback and legacy migration. A second test
can migrate an isolated copy of the installed database by setting
`DISKHISTORY_MIGRATION_SOURCE` to its path before running tests. `build.ps1 package`
creates the UI and console executables in `dist/VolumeTrail/`.

The 5-minute incremental runtime goal is a target, not a timeout. Full scans
run to completion unless stopped. Runtime measurements and visual UI acceptance
are recorded separately from the automated tests.

For the released v0.3.0 source, [Windows CI #39587285000](https://github.com/BW1145/VolumeTrail/actions/runs/39587285000)
passed all-target tests and the isolated NTFS cleanup checks. The
[NTFS validation scope](ci-ntfs-validation.md) distinguishes CLI lifecycle checks
from actual Windows login-task and desktop interaction acceptance. Earlier
[verification notes](verification-0.3.0.md) are pre-release snapshots.
