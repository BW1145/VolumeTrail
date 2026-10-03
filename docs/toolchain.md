# Build Toolchain

The project uses Rust's Windows GNU target with LLVM-MinGW's Clang driver.
Development tools, package caches and build output are under this workspace.

- LLVM-MinGW release: 20260908, msvcrt x86_64
- Source: https://github.com/mstorsjo/llvm-mingw/releases/tag/20260908
- Archive SHA-256: `341cc9786b54956467ac19a02fdb00749307eac133da56108f643e4189db810b`
- Build command: `./build.ps1 test` or `./build.ps1 release`

## Diagnostic Record

On 2026-09-08, 360 quarantined two entries under the development-only
`w64devkit` directory with a truncated `Win64/Heur.Generic` detection name.
Compilation failed with access denied. The complete affected filenames were
not visible in the supplied screenshot.

On 2026-09-12 the original w64devkit archive SHA-256 was verified against the
GitHub release asset digest:
`9208c19755cd4964b7915b9afcf02c66d493a4c870c4b3e83f6c538d9c1237a5`.
This verifies archive provenance, not a malware verdict. Quarantined entries
remain quarantined. The build script uses the LLVM toolchain above.

On 2026-09-26 at 09:43:01, `./build.ps1 release` stopped when 360 reported
`HEUR/QVM202.0.4FC9.Malware.Gen` for
`target/release/build/proc-macro2-a9376292ca3ec434/build-script-build.exe`.
360 removed that temporary build executable and blocked Cargo from starting it.
The cached `proc-macro2 1.0.107` crate archive has SHA-256
`985e7ec9bb745e6ce6535b544d84d6cd6f7ad8bd711c398938ae983b91a766d9`,
matching `Cargo.lock`. Its `build.rs` performs compiler capability probes.
These source checks do not establish a verdict about the generated executable.
The quarantined file has not been restored or allowlisted; the release build
remains incomplete. The debug build and its tests were successful before this
release attempt.
