# UI polish shutdown handoff

Historical checkpoint: [PR #1](https://github.com/BW1145/VolumeTrail/pull/1) is now merged
and [v0.3.0](https://github.com/BW1145/VolumeTrail/releases/tag/v0.3.0) is published.
Use the [README](../README.md) for current instructions. The branch restrictions
and continuation steps below describe the pre-release checkpoint, not a request
to rebuild or republish the existing release.

Branch: `codex/overview-cleanup`, repository `BW1145/VolumeTrail`.
This checkpoint preserves the four changes requested after independent visual review.
Do not merge main, publish a Release, update the installed app, register a login task,
or enable any rule on real user data. The original checkout remains untouched on main.

## Saved changes

- Age row: “至少 [N] 天未修改”; the zero-day explanation has its own short line.
- Rule, candidate and result paths reuse middle elision, fitted using the actual font width,
  retaining the ending and full-path tooltip.
- One-time action says “删除本次匹配文件”.
- Recent result summaries include estimated allocation of deleted files.
- The optional capture fixture adds long paths, expanded candidates, acknowledged actions,
  scrolled preview/result sections, synthetic partial failure and interruption states.
  These are presentation fixtures, not system mouse/keyboard or OS-failure evidence.
- A regression renders a narrow egui viewport and verifies path width and filename retention.

## Verification state at checkpoint

The previous accepted code head `68e89dc983fd0ff3189b421668734bec967eebc2` has successful
CI run https://github.com/BW1145/VolumeTrail/actions/runs/39585483200 .
Its 68 default tests and actual isolated NTFS VHD integration passed.
The existing local dist package/manifest still describe that earlier head; do not relabel it.

The new path regression first exposed an incorrectly sized test viewport. That viewport was
corrected and the full local rerun was started. The push is prioritized before shutdown;
check the new commit's CI rather than assuming this checkpoint passed all checks.
The expanded capture feature has not yet been compiled or visually inspected at checkpoint.

## Continue from GitHub

1. Fetch/checkout `origin/codex/overview-cleanup`, read this file and inspect the branch head.
2. Inspect its exact-head Windows Actions run; resolve failures without weakening the tests.
   `./build.ps1 test -CargoArgs '--locked','--all-targets'` is the normal Windows command.
   The workflow bootstraps the pinned Rust/LLVM toolchain on a fresh Windows runner.
3. Build the optional harness and run `./tests/render-review.ps1`. On the existing isolated
   workstation use `../dev-build.ps1 build -CargoArgs '--locked','--offline','--features','ui-capture','--example','ui_capture'`
   followed by `./tests/render-review.ps1 -SkipBuild`; the external wrapper only reuses local tooling.
4. Inspect all nine `evidence/ui/polish-*.png` images, fix clipping/scroll positioning if needed,
   and request the independent visual review through the parent. Retain original screenshots.
   Never use forbidden native-input workarounds. An offscreen GL renderer may be unavailable
   in a cloud session; if so report that limitation and wait for an available Windows renderer.
5. Commit any fixes and require exact-head CI again, including the CI-created isolated VHD.
   Rebuild the candidate package and synchronize manifest/hashes/evidence to that final head.

Pending acceptance remains: actual desktop mouse/keyboard/scroll, actual Windows login-task
trigger (CLI lifecycle is tested), real provider cloud placeholders and external production
history-copy replay. Library cross-thread image reading returned scope_violation 403 even
after an authorized retry. Do not try alternate paths around that restriction.

No shutdown, power, sleep or system security settings were changed.
