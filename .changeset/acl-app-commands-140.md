---
'ikenga-desktop': patch
---

Fix the dead iyke FE⇄backend channel: grant the app's own commands in the Tauri
ACL (#140), and stop the smoke gate confusing "bridge broken" with "app is on
onboarding" (#147).

**#140.** In v0.8.0 no terminal could spawn, the shell never published its
state, and `iyke` could not drive the app. Cause: `7ccf4e87`, a Dependabot
advisory sweep, moved us from tauri 2.11.0 to 2.11.5. The ACL gate in
`tauri/src/webview/mod.rs` changed in 2.11.2 from

    if (plugin_command.is_some() || has_app_acl_manifest)

to

    if (plugin_command.is_some() || has_app_acl_manifest || !is_local)

— hardening so that "remote content can never reach custom commands unless an
explicit `remote` capability has been configured for them". The shell's main
window loads remote content by design: it is built with
`WebviewUrl::External("http://localhost:<viewer_port>/")` so the shell is
same-origin with the viewer-server that serves `/__viewer/*`. From 2.11.2 on,
that made `is_local` false and put all 238 of the app's `#[tauri::command]`s
behind an ACL that granted none of them. Every invoke was rejected with
`Command <name> not allowed by ACL` — `pty_spawn`, `iyke_set_shell`,
`iyke_dom_done`, `settings_get`, `detect_system`, the lot. Dev builds were
unaffected because there the window URL equals `build.devUrl`, so the origin is
Local; the regression was therefore invisible in `tauri dev` and to every
compile-time gate. v0.7.2 predates the bump, which is why it works; v0.7.3 was
the first build to carry it and was never shipped.

Fixed by adding `src-tauri/permissions/app-commands.toml`, which declares the
app ACL manifest and grants `allow-app-commands` to the `main` and `detached-*`
windows. `pkg-*` child webviews — the pkg-browser's arbitrary partner portals,
whose capability has a deliberately wide-open `remote.urls` — get only
`allow-iyke-browser-reply`, so the tauri hardening is kept rather than worked
around: partner-site JS now genuinely cannot reach `pty_*`, `secrets_*` or
`db_exec`, which before 2.11.2 it could.

Because the file exists, app commands are now ACL-gated on local origins too, so
an omission fails identically in `tauri dev` and in a release build. On top of
that, `bun run test:acl-parity` asserts that the grant list and
`tauri::generate_handler!` are the same set, and runs first in CI — it takes
milliseconds and is the check that would have caught this.

**#147.** The launch smoke gate could not tell a dead bridge from a healthy app
parked on the onboarding wizard: both produce `/iyke/dom` timeouts and a null
`shell.mode`/`route`, because onboarding renders edge-to-edge without the
`Workspace` that mounts `useIykeBridge`. That confound invalidated an entire
session's reproduction of #140. Onboarding now mounts the bridge itself and
publishes a literal `route: '/onboarding'` — deliberately not via
`useIykeShellSync`, which derives the route from the focused pane and on a first
run would confidently report `/`. The gate checks `/iyke/state` before probing
`/iyke/dom`, and on an onboarding route exits 2 with `INCONCLUSIVE` and an
explicit "the bridge is alive, the seed did not take" rather than blaming #140.
