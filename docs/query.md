# Querying VolumeTrail

Run commands with `VolumeTrail-cli.exe` from the portable folder. Query commands
use the existing SQLite index; they do not start a scan or need elevation.

```powershell
.\VolumeTrail-cli.exe query summary --drive C --limit 20
.\VolumeTrail-cli.exe query growth --drive C --from 6 --to 7 --depth 5 --json
.\VolumeTrail-cli.exe query folder --drive C --path 'C:\Users\Example\AppData\Local' --from 6 --to 7 --json
.\VolumeTrail-cli.exe query largest --drive C --kind folders --limit 50 --json
.\VolumeTrail-cli.exe query largest --drive C --kind files --offset 50 --limit 50
```

`summary` lists scan points newest first. `growth` uses the two latest points
when `--from` and `--to` are omitted. `--path` narrows growth to a folder and
defaults to its immediate child level. `folder` shows the current index; with
`--from` or `--to`, it also shows the interval's child growth and extension
distribution. `largest` reads current-index directory or file sizes. Limits
range from 1 to 5000; the UI uses pages of 50 or 100.

JSON sizes are bytes. `allocated_delta_bytes` is signed: positive means growth,
negative means release. `moved_delta_bytes` is the signed portion attributable
to a move. Two ends of one same-volume move sum to zero. `intervals`,
`available_aggregate_intervals`, `complete_aggregate_intervals` and
`detail_intervals` report comparison coverage.
Incomplete coverage means the returned ranking includes only retained data.

`folder.comparison` includes the selected directory's `allocated_delta_bytes`,
`direct_delta_bytes` and `moved_delta_bytes`, even when it has no child directories.
Direct changes are aggregates recorded at this directory level, including whole
directory movement. Extension totals require retained per-file details and can
therefore have shorter coverage than directory totals.

A directory absent from the current index returns `current_index_present:false`
and null current sizes; its historical comparison is still available. Current
index absence does not prove that the live directory is absent.

New scan summaries include `performance`: phase durations, busy-pause time,
journal event count, unique changed identities, NTFS entry read attempts, CPU
time, whole-machine average CPU percentage and process peak working-set bytes.
The average includes busy pauses. The peak belongs to the worker's lifetime,
including previously scanned drives during the same automatic run. Old scan
points have null performance; no measurements are reconstructed retroactively.

Read-only SQL examples against `data/history.db`:

```sql
SELECT id,root,volume,sampled,finished,total-free AS used_bytes,free,
       details,aggregate
FROM scans ORDER BY id DESC;

SELECT path,SUM(allocated_delta) AS growth_bytes,
       SUM(moved_delta) AS moved_bytes
FROM folder_changes WHERE scan > 6 AND scan <= 7
GROUP BY path ORDER BY growth_bytes DESC LIMIT 50;

SELECT n.name,f.allocated,f.files
FROM folders f JOIN nodes n ON n.volume=f.volume AND n.id=f.id
WHERE f.volume='ntfs:...' AND n.is_dir=1
ORDER BY f.allocated DESC LIMIT 50;
```

`scans.aggregate`: 2 means complete new-format interval, 1 means a legacy
delta-based approximation, and 0 means its directory aggregate is unavailable.
`scans.details` records whether per-file changes remain. Scan IDs are never
reused. `volume` identifies the filesystem separately from the drive letter.
