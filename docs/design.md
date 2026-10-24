# VolumeTrail

Current contract: v0.3.1, including the cleanup extension authorized on 2026-10-21.
The original disk-growth recorder was authorized on 2026-09-08.

## Product contract

A portable Windows disk-growth recorder with user-configured file cleanup. Default volume is C:. Users can add
other NTFS volumes. Application-owned configuration, index, history and logs
live in data/ beside the executable. No file contents are backed up.

- One automatic scan after startup/login, then the worker exits.
- No resident service, game detection, periodic daytime scans or shutdown scan.
- One to two minutes is the target; five minutes is a performance goal, NOT a timeout.
- Manual cancellation preserves the last complete scan.
- Background work yields while CPU or disk pressure is high.
- First scan uses the NTFS MFT; following scans consume USN records when continuous.
- A journal reset/gap causes a new baseline, not a fabricated incremental result.
- Changes distinguish creation, deletion, growth, shrinkage and rename/move.
- Keep logical size and allocated bytes distinct; count hard-linked data once.
- Show unaccounted volume usage instead of forcing file totals to match volume totals.
- History retention uses lossless compression for older file details. Scan
  timepoints, file details and folder/type aggregates persist until explicitly
  removed by the user. The current index avoids duplicating fields in JSON.
- UI includes overview and long-term trend filters, folder browser and largest
  items, change history and drilldown, settings, manual scan, cancellation and
  opening/selecting the target in Windows Explorer.
- File cleanup is limited to explicitly configured local directories, filename
  patterns, recursion and modification age; saving/editing leaves rules paused.
  Preview expires after 15 minutes. One-time execution and automatic enablement
  require separate explicit user authorization. Deletion is permanent: no undo,
  recycle bin or file-content backup. Cancellation only stops subsequent files.
- Enabled rules run after a successful login scan, at most once per rule per
  local day. Manual scan does not clean. Enabling a rule does not create a task
  or add its volume to the configured scan list.
- Cleanup results retain rule snapshots, bounded item outcomes and estimates.
  Execution outcome and index verification are separate; a failed verification
  does not mean deletion failed. Interrupted intentions are not replayed.
- No cloud service, account or telemetry.
- Normal automatic completion is silent; failed/cancelled runs are visible in UI.
- Startup configuration is opt-in in the UI and requires elevation when needed.

## Architecture

Rust application with a worker process and an egui/eframe report UI. The manual
scan path scans only; the login worker also runs enabled cleanup rules.
SQLite stores current file records, scan summaries, per-file deltas and folder
interval aggregates plus cleanup rules, previews and execution records.
`VolumeTrail-cli.exe` exposes text/JSON queries and the structured
[automation interface](automation.md) for rule management and cleanup.
The NTFS parser is reused from an existing open-source library and validated
against real-volume fixtures. Windows APIs provide USN access, file identity,
allocation, task scheduling and Explorer integration. No custom kernel driver.
Worker and UI coordinate cancellation via a local flag file and a global scan
lock, also used by cleanup writes. UI does not require administrator rights for
reading reports. Cleanup uses current-process permissions without automatic
UAC or ACL changes; raw NTFS scans/verification need administrator access.
The existing login task runs at the highest available privilege, inherited by
its enabled cleanup rules. See the [README](../README.md) for user instructions.

## Correctness

Journal checkpoints advance only with committed scan state. A baseline saves a
journal cursor from before enumeration and replays intervening changes. Read
failures are reported and cannot become deletions. Volume identity is independent of
drive-letter reuse. Directory identities permit subtree moves without treating
unchanged descendants as deleted. Full scans and incremental updates are
published transactionally. Only complete scans participate in comparisons.

Cleanup pins ancestor directory handles and rechecks file identity and metadata
before deleting only previewed candidates. Protected scopes, reparse points,
multi-linked and special-attribute files are excluded. Index verification uses
incremental scanning only; an unavailable journal leaves statistics unverified.

## Verification

Unit tests cover delta accounting, identity deduplication, rename and deletion,
journal-gap selection, retention and cancellation rollback. Storage integration
tests cover directory moves, nested resize, deletion and carry-forward of scan
intervals, current-index queries and migration. An isolated copy of the installed
database is used for schema migration acceptance. Real-volume scanning and
native UI interaction require separate host acceptance.

## Reference projects

- https://github.com/chuunibian/delta (snapshot comparison UX, MIT)
- https://github.com/windirstat/windirstat (NTFS accounting reference, GPL-2.0)
- https://github.com/xangelix/edirstat (native scan/UI separation, MIT)
- https://docs.rs/ntfs-reader (MFT/USN reader candidate)

Code reuse must retain applicable licenses. No GPL code will be copied without
making the resulting licensing explicit.
