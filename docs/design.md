# VolumeTrail

Status: implementation authorized by the user on 2026-09-08.

## Product contract

A portable Windows disk-growth recorder. Default volume is C:. Users can add
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
- History retention uses a configurable 512 MiB default database budget. Scan
  timepoints and the current index remain; file detail and then folder aggregates
  are removed oldest first when the budget is exceeded.
- UI includes overview and long-term trend filters, folder browser and largest
  items, change history and drilldown, settings, manual scan, cancellation and
  opening/selecting the target in Windows Explorer.
- No cleanup/deletion of user files, cloud service, account or telemetry.
- Normal automatic completion is silent; failed/cancelled runs are visible in UI.
- Startup configuration is opt-in in the UI and requires elevation when needed.

## Architecture

Rust application with a scan-only worker mode and an egui/eframe report UI.
SQLite stores current file records, scan summaries, per-file deltas and folder
interval aggregates. `VolumeTrail-cli.exe` exposes read-only text/JSON queries.
The NTFS parser is reused from an existing open-source library and validated
against real-volume fixtures. Windows APIs provide USN access, file identity,
allocation, task scheduling and Explorer integration. No custom kernel driver.
Worker and UI coordinate cancellation via a local flag file and a global scan
lock. UI does not require administrator rights for reading reports.

## Correctness

Journal checkpoints advance only with committed scan state. A baseline saves a
journal cursor from before enumeration and replays intervening changes. Read
failures are reported and cannot become deletions. Volume identity is independent of
drive-letter reuse. Directory identities permit subtree moves without treating
unchanged descendants as deleted. Full scans and incremental updates are
published transactionally. Only complete scans participate in comparisons.

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
