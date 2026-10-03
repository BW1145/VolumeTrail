# VolumeTrail v2 implementation

## Product

Portable Rust + egui app with a separate scan process. The scan starts at login
when enabled, processes the configured NTFS volumes sequentially, then exits.
SQLite and configuration live beside the executable in `data/`.

## Delivered behavior

- Scan summaries retain sample time, completion time, disk capacity/free space,
  indexed bytes and file count. The chart can show 7, 30, 365 days or all points.
- History budget first removes per-file detail and then directory aggregates,
  oldest first. It measures the pages used by these history tables and their
  indexes; the current index, scan summaries, performance and migration backups
  have separate storage totals. Small scan summaries and the current index remain. The UI can
  clear selected details or delete selected scan points.
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
- Opening an older database creates a consistent backup and migrates its schema.
  Legacy changes are aggregated to the extent their saved deltas permit.

## Verification

`build.ps1 test -CargoArgs '--lib','--test','store'` covers accounting, movement, history deletion/carry, retention,
current-index queries, cancellation rollback and legacy migration. A second test
can migrate an isolated copy of the installed database by setting
`DISKHISTORY_MIGRATION_SOURCE` to its path before running tests. `build.ps1 package`
creates the UI and console executables in `dist/VolumeTrail/`.

The 5-minute incremental runtime goal is a target, not a timeout. Full scans
run to completion unless stopped. Runtime measurements and visual UI acceptance
are recorded separately from the automated tests.
