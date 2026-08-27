---
'ikenga-desktop': patch
---

Move the headless `ikenga-server` daemon into its own crate so it is no longer
bundled into the desktop app — fixing macOS universal releases, which had been
failing outright.

Tauri's bundler enumerates every `[[bin]]` target of the Tauri crate and copies
each one into the app bundle, with no config to opt a binary out. While the
daemon was a second `[[bin]]` of `ikenga-desktop` it was silently shipped inside
every `.app`, `.deb`, `.AppImage` and `.exe`, and on `universal-apple-darwin` it
broke the build: Tauri `lipo`s only the *main* binary into the universal target
directory, so the bundler then looked for a
`target/universal-apple-darwin/release/ikenga-server` that was never created.

That is what left v0.7.3 a draft with 7 of 10 assets. The `[[bin]]` arrived in
`5c8d19a7` (#98) and is not in v0.7.2, which is why the pipeline was green until
then — and why every subsequent release would have failed the same way.

`src-tauri/server/` is now its own crate and a workspace member, depending on
`ikenga-desktop` with `default-features = false`. The bundler never sees it, and
the daemon ships the way it is actually deployed: built by
`scripts/server/deploy.sh` and run under systemd on its own host. Desktop
bundles lose a binary they never needed.

`scripts/sync-version.mjs` now propagates the version into the new crate and its
`Cargo.lock` entry, so `ikenga-server --version` stays in lockstep; it fails
loudly if either pattern stops matching.
