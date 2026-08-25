---
'ikenga-desktop': patch
---

Bump the pinned Bun runtime from 1.3.14 to 1.4.0, and add native
`windows-aarch64` (Windows on ARM) as a supported Bun target.

`BUN_VERSION` and the per-target sha256 table in `src-tauri/src/runtime.rs` are
the source of truth for both the runtime fetch path and the system-Bun
acceptance floor (`IKENGA_BUN_PATH` → system Bun ≥ pin → SHA-pinned fetch).
`scripts/fetch-bun.sh` mirrors both, and the `pin_table_matches_fetch_bun_script`
test asserts the two stay in lockstep — so this updates them together.

All five per-target sha256s come from the published `SHASUMS256.txt` for
`bun-v1.4.0`; the linux-x64 zip was additionally downloaded and hashed locally
to confirm the manifest, and `fetch-bun.sh --target linux-x64` was run
end-to-end (download → sha verify → unzip → `bun --version` reports 1.4.0).

`windows-aarch64` is new: `BUN_TARGET` previously fell through to `unsupported`
on Windows/ARM, so those hosts never got a fetched Bun and fell back to PATH.
Its sha256 is covered by the lockstep test — verified by deliberately corrupting
it and confirming the test fails naming that target.

Note the raised floor: a system Bun older than 1.4.0 is now rejected and falls
through to the fetched copy.
