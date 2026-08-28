# ikenga-desktop

## 0.8.2

### Patch Changes

- ceb9398: Stop pointing terminals at a throwaway `CLAUDE_CONFIG_DIR` overlay, so `claude`
  in an Ikenga terminal uses your real `~/.claude` (#149).
  
  Every PTY the shell spawned got `CLAUDE_CONFIG_DIR` set to
  `$XDG_RUNTIME_DIR/ikenga/claude-overlay`, with the user's real assets symlinked
  in. Two of those symlinks silently never happened: the builder looked for
  `~/.claude/credentials.json` and `~/.claude/.claude.json`, while the real files
  are `~/.claude/.credentials.json` and `~/.claude.json` (in `$HOME`, not inside
  `.claude/`). Both `if target.exists()` checks failed and skipped, so `claude`
  started against a config dir with no credentials and no `projects` map — asked
  you to log in and to trust the folder, then wrote a SECOND config there. On a
  real machine that meant a 2 KB / 0-project config shadowing a 116 KB /
  23-project one, and because `$XDG_RUNTIME_DIR` is wiped on reboot, a fresh login
  after every restart.
  
  The overlay's one legitimate purpose was seeding `settings.json` so the shell
  could inject its statusline and hooks invisibly. That never worked either:
  `ensure_claude_overlay_dir` hardcoded `port: 0` and no token, so it wrote
  `curl … http://127.0.0.1:0/iyke/statusline/event` and the same for hooks, on
  `PreToolUse` / `PostToolUse` / `SessionStart` / `SessionEnd` /
  `UserPromptSubmit`. A failing curl on every tool call is worse than no
  injection, so nothing of value is lost by removing it.
  
  Terminals now use the user's real config natively — which is what the
  2026-07-20 overlay retirement already did for the chat path and missed here.
  `src/pty/overlay.rs` and the two `configure_overlay_*` writers are deleted;
  `write_ide_lock_file` is kept but marked uncalled, because its only caller was
  the overlay passing `port: 0` and a literal `"ikenga-token"`, meaning IDE
  lock-file discovery has never actually worked. Re-wiring it must pass the live
  iyke bridge port and bearer token.
  
  A caller that sets `CLAUDE_CONFIG_DIR` explicitly (via `opts.env` or the app's
  own environment) is still honoured.

## 0.8.1

### Patch Changes

- 944031f: Fix the dead iyke FE⇄backend channel: grant the app's own commands in the Tauri
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
- 4ee85fe: Add a launch smoke gate to the release pipeline, and stop the iyke bridge
  swallowing its own failures.
  
  **The gate.** Every check in the pipeline verified that the code compiles and
  its units pass; none of them ever started the app. That is how v0.8.0 shipped
  with a dead iyke FE⇄backend channel and no terminal able to spawn, while
  typecheck, both cargo checks, 786 Rust tests, 804 frontend tests, CI and all
  four release legs were green (#140). `scripts/launch-smoke-gate.ts` now launches
  the built binary on the Linux release leg under `xvfb-run` and probes
  `GET /iyke/dom`, which round-trips backend → FE listener → `invoke` → backend
  and so fails on exactly that class of break. Verified against both artifacts:
  passes on v0.7.2, fails on v0.8.0 with `iyke://dom-request timed out after
  5000ms`. A smoke failure fails the build job, so the release stays a draft and
  never becomes `Latest`.
  
  Two things the gate has to work around, both of which would otherwise make it
  lie. A virgin data dir is a first run, and a first run renders `/onboarding`
  without the Workspace chrome that mounts the bridge — so it boots once to let
  the app migrate its database, seeds onboarding as complete, then relaunches and
  probes that second boot. And `control.json` outlives the process it describes,
  so it is deleted before the relaunch; otherwise the probe dials a dead port and
  reports a connection error indistinguishable from a hung frontend.
  
  **Diagnosability.** Every `.catch(() => {})` around an iyke resolve now names
  the failing command, so "the listener never registered" and "the listener ran
  and could not answer" stop producing an identical backend timeout. The console
  instrumentation is installed at frame 0 in `main.tsx` rather than by
  `useIykeBridge`, which removes the circularity that left `/iyke/logs` empty and
  healthy-looking precisely when the bridge was broken.
  
  **Capability snapshots.** `pkg_capability_snapshots` rows are now torn down with
  the install record, so a dev mount's implicit approval can no longer outlive the
  mount and silently pre-approve a later real install of the same pkg id (#144).

## 0.8.0

### Minor Changes

- d3f02ae: Remote-access wave 2: the headless server surface, plus two terminal fixes and
  a credential-leak fix.
  
  **Security — the daemon no longer leaks its own credentials into every PTY.**
  `pty::spawn_inner` inherited the parent environment wholesale, and
  `bin/ikenga-server.rs` has clap read `IKENGA_AUTH_TOKEN` from the environment,
  which systemd populates from `/opt/ikenga/.env`. Anyone who could open one
  terminal could read the bearer token that grants terminals — a credential that
  outlives the session and survives revoking the client. Privilege *persistence*
  rather than escalation (holding the token already implies shell access), which
  is why this ships as a fix.
  
  Two layers: `ikenga-server` scrubs `IKENGA_AUTH_TOKEN` / `IKENGA_VAULT_KEY`
  from its own environment once clap has read them, and `pty::is_host_only_env`
  denylists those plus `IKENGA_SECRET_*`. The `env_clear()` before the inherit
  loop is load-bearing and not obvious: `CommandBuilder` seeds itself from
  `std::env::vars_os()` at construction, so it inherits by default and skipping a
  key in the loop leaves the already-inherited copy in place. Agent credentials
  (`ANTHROPIC_API_KEY` and friends) are deliberately still inherited — a remote
  terminal holding the box's agent auth is the point of the design. The split is:
  unprefixed env reaches agent CLIs, `IKENGA_*` host credentials never reach a
  shell.
  
  **Terminal labels can no longer collide.** `/iyke/terminal/spawn` rejected a
  duplicate label by scanning for `status == "running"`, which cannot see a
  terminal that has been spawned but has not yet reached `running` — so two
  concurrent spawns with the same label both passed, roughly 1 in 5. Replaced
  with `PtyManager::reserve_label`, which takes the name under a lock and holds
  it until `set_label` succeeds; `LabelReservation`'s `Drop` releases it on every
  early return so a failed spawn doesn't strand the name.
  
  **Popped-out terminals no longer fight over the PTY size.** Two windows
  attached to one PTY at different sizes drove conflicting resizes and corrupted
  the reflow. Attached non-owning viewers now skip `ptyResize` while their window
  is unfocused (active viewer wins), and an unchanged size is a no-op on the Rust
  side.
  
  Also lands the headless server surface behind it: static pkg serving with
  lexical-then-canonical traversal checks, the fs-socket transport, and the
  symlink-escape fix in the watcher allowlist.

### Patch Changes

- fe3554e: Bump the pinned Bun runtime from 1.3.14 to 1.4.0, and add native
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
- 92d615a: Move the headless `ikenga-server` daemon into its own crate so it is no longer
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

## 0.7.2

### Patch Changes

- 25c5085: Point the registry and primitives catalog at `registry.ikenga.dev` instead of the
  GitHub-hosted `royalti-io.github.io` URL. Same content, same signing key — a
  hostname we own, so the registry no longer depends on which GitHub org holds the
  repo. Kept in lockstep with `@ikenga/cli`.

## 0.7.1

### Patch Changes

- 1485bb4: Fix the approve gate silently discarding Reject / Approve / Retry clicks.
  
  Two independent bugs combined into a single silent failure: `pausedDraftFromRow`
  never copied `row.id` onto the view model, so every action invoked with
  `draftId: undefined`, and `pa_actions_reject`'s WHERE clause refused the
  `failed` rows the panel actually offers Reject on. The panel optimistically
  marked the row resolved and removed it either way, so the gate looked like it
  had worked while the database was untouched.
  
  Cherry-picked to main from `spike/sandbox-containment`, where it was blocked
  behind unrelated artifact-sandbox work.

## 0.7.0

### Minor Changes

- 965fd14: The in-app chat surface is gone, replaced by Chi — a runtime for driving coding
  agents through the shell instead of a chat pane bolted onto it. **This is
  user-visible and breaking**: anyone relying on the old in-shell chat pane will
  find it removed, not migrated. The chat panel, its backend session store, and
  `chat_sessions`/`chat_user_turns` are deleted outright (migration 0060), along
  with the standalone AI-elements component library and the unused Gemini ACP
  engine path it depended on. If you had conversations parked in the old chat
  pane, they do not carry forward.

  In its place:

  - **Chi agent runtime.** New `chi_run` / `chi_resume` / `chi_cache` plumbing
    (migration 0059) drives real coding-agent sessions from the shell, with a
    local cache so history survives restarts. The Claude Code engine merges its
    native `~/.claude/projects` sessions into `chi_list`, so sessions started
    outside Ikenga show up alongside ones started inside it.
  - **Multi-engine support.** Beyond Claude Code, `chi_run` now has real parity
    for a Codex engine, a stub for `cursor-agent`, and a new Antigravity engine —
    the legacy Gemini ACP path is retired in the same pass.
  - **Terminal multiplexer + tmux persistence.** Chi runs live in a real
    multiplexed terminal backed by tmux sessions, so a run's terminal state
    survives disconnects instead of dying with the pane.
  - **iyke HTTP bridge for Chi.** `/iyke/chi/{run,resume,status,list,cancel}`
    lets an external controller drive Chi the same way it already drives
    terminals and panes.
  - **Headers-only mailbox index** (migration 0058, `email_index`) for faster
    mail lookups without pulling full message bodies.
  - **Telemetry consent surface removed** along with the chat UI it was attached
    to.

  Fixed:

  - The sidebar's active section now re-syncs to whatever pkg route the focused
    pane is actually on, on both navigation and cold start — deep links and
    restored sessions no longer snap back to the generic "app" mode and lose
    their pkg-specific side menu.
  - The artifact viewer now sends `Cache-Control: no-cache`, so editing an
    artifact file no longer leaves the viewer showing a stale cached copy.

## 0.6.2

### Patch Changes

- b6328c4: Agents driving Ikenga over the iyke bridge can now create the terminals they
  work in, read what their timers fired, and link that runtime work to the durable
  task board — the three gaps that made the multi-agent story unreachable from
  outside the app.

  - **Terminal lifecycle** — `POST /iyke/terminal/{spawn,kill}`. Spawn round-trips
    through the frontend so an agent's terminal is an ordinary visible tab you can
    watch, pop out, or take over, rather than an invisible Rust-local PTY. The
    follow-up lease addresses a concrete `pty_id`, since one terminal can own
    several PTY records and taking the first match could lease a dead one.
  - **Agent inbox** — `GET /iyke/agent/inbox` + `POST /iyke/agent/inbox/ack`.
    Timers had been writing to `iyke_agent_inbox` all along with no way to read it,
    which made `/iyke/timer/schedule` a no-op for agents. Scheduling against an
    unregistered agent now returns an actionable 400 instead of a raw foreign-key
    error.
  - **Task board link** — migration 0057 adds a nullable `iyke_todos.task_id`
    (deliberately no foreign key, so deleting a task orphans a runtime todo rather
    than failing), plus `/iyke/task/{list,create,update,complete}`.
  - **Email actions** — migration 0056 adds `email_actions` +
    `email_triage_cursor`, with proposal lifecycle columns keeping proposals,
    approvals, and executions in one audit trail.

  Supporting UX: terminal tabs are named for what they run and where
  (`claude · shell`) instead of every tab reading "Terminal"; dropped OS files
  route to the surface under the cursor, inserting a shell-quoted path in a
  terminal or attaching an image in the composer; the updater holds at `installed`
  and never auto-relaunches, so a restart can't discard in-flight work; detached
  windows can set their own OS title so pop-outs are distinguishable in the window
  list.

## 0.6.1

### Patch Changes

- 4cd5ad4: Reopening Ikenga while it's already running now focuses the existing window
  instead of launching a second copy. Previously a double-clicked launcher (or an
  app reopen during an update) forked a whole second instance — its own SQLite
  handle, iyke bridge, and pkg kernel — which then raced the running instance on
  the shared database. Added `tauri-plugin-single-instance`, registered first so
  the second process forwards its launch to the running window and exits.

## 0.6.0

### Minor Changes

- 3c60f59: Terminal ergonomics, app-wide zoom, and a collapsible sidebar.

  - **Shift+Enter inserts a soft newline** in the terminal instead of submitting.
    A bare terminal can't distinguish Shift+Enter and sends a carriage return for
    both, so multi-line input in the `claude` CLI (and other TUIs that accept it)
    didn't work; Shift+Enter now sends a line feed the app reads as a literal
    newline — the same distinction `/terminal-setup` configures in iTerm2 / VS Code.
  - **App-wide zoom** (⌘/⌃ with `=` / `-` / `0`). One level for the whole shell —
    chrome, panes, pkg iframes, and the xterm canvas — applied at the webview
    level so text stays hinted and the terminal re-fits its PTY correctly. A
    discrete ladder means zoom-out then zoom-in always returns to a crisp 1.0.
    The level persists and syncs across detached pop-out windows.
  - **Collapsible sidebar.** ⌘B toggles it, and clicking the already-active
    activity-bar item collapses/reopens it (clicking a different item always
    reopens). The collapsed state persists across restarts.
  - **`/iyke/sidebar` verb** (`toggle` | `open` | `close`) drives the same state
    over the iyke bridge, and the sidebar's collapsed state is now reported in
    `/iyke/state` so it's observable, not just actuate-only.

## 0.5.1

### Patch Changes

- 209710e: Fix in-app updates reading as a mid-process crash on Linux. An app update now
  holds at an explicit "installed — Restart to finish" state with a Restart
  button, instead of relaunching the moment the install completes and tearing the
  window down out from under you (which, with the download bar frozen at the
  elevated `dpkg` step, was indistinguishable from a crash even though the update
  had actually applied). The opt-in "install app updates automatically" setting
  keeps relaunching on its own.

  Note: this smooths the _next_ update — an update installed by an older build
  still relaunches the old way; the Restart-to-finish flow takes effect for
  updates applied from this build onward.

## 0.5.0

### Minor Changes

- ad7a62d: Retire the per-session `CLAUDE_CONFIG_DIR` overlay; chat sessions now use claude's own discovery.

  **Chat / transcripts**

  - Chat sessions reach exact parity with a plain terminal: 143 skills, 33 agents, 298 commands, 23 MCP servers (was 129 / 33 / 271 / 8 under the overlay).
  - Transcripts land in `~/.claude/projects` and are resumable with `claude --resume`, both inside and outside the app. 19 pre-existing transcripts were migrated.
  - Transcript retention pinned rather than left unset, so the 30-day sweep no longer eats history.
  - Abandoned threads are GC'd on close, safely under concurrent mounts.
  - Claude child processes shut down gracefully on SIGTERM.

  **Terminal**

  - Pop-out no longer shows blank scrollback: buffered output is held until a live chunk actually lands, and the PTY attach seam is closed in Rust rather than deduped in JS.
  - PTY reader-thread panic guard plus a live-session cap.
  - Terminal PTY is disposed and the xterm/webgl context evicted on tab close.
  - A SIGWINCH repaint nudge is issued when a terminal is attached into a
    detached pop-out, and again when the pane is reclaimed by the origin window,
    so a full-screen TUI is prompted to redraw at the geometry it is actually
    being displayed at. This does not repair scrollback that was already written
    at the previous geometry — raw-replay rewrap remains structural, and
    line-mode shells are unaffected by the nudge.

  **Pkgs / kernel**

  - Settings-secret env is injected into sidecars from Stronghold at both spawn sites.
  - Pane lifecycle: xterm cache, stable tab keys, pooled pkg iframes, pkg-MCP event relay.
  - Two-line pkg menu header with subtitles on `PkgMenuItem`.
  - Studio: nested-route subresource inlining, `host.openFolder` trust wiring, dev-reload sidecar reap, per-folder trust gate.

  **Fixes**

  - `~/`-rooted paths are now detected by the terminal path linkifier, unblocking `resolvePath`'s previously unreachable tilde-expansion branch.
  - Artifact `file:` data sources resolve against the artifact mount instead of falling through to mock.
  - Dock ⌘J can no longer strand the dock in `hidden`.
  - `main.tsx` can no longer brick on a failed boot module load.
  - Revived the two dead `/iyke/logs` filters.

## 0.4.0

### Minor Changes

- bb6b519: Tab + artifact context menus with pin-to-sidebar, first-party host.openArtifact verb (sender-pane resolution), multi-window follow-ups (focus-changed emission, focused-window screenshots with main fallback, label uniqueness, webview leak cleanup, registry liveness), detached-terminal scrollback replay, and operator identity threaded through hostContext.

## 0.3.0

### Minor Changes

- 804c7a0: Multi-window Phase 1 — thin-window substrate + Flavor C (detach single surfaces).

  A window is now a thin webview rendering a declared `surface_set`, backed by the
  shared Rust core and coordinated by Tauri events (no client-cache mirroring).
  Adds the `G-WINDOW-MODEL` contract (`@ikenga/contract/window`), a Rust window
  registry (`window_spawn`/`close`/`list`), per-window-aware pkg-pane parenting
  (de-`"main"`'d), a thin `boot/detached` FE entry with per-window state isolation,
  and **pop-out** detached windows for **chat**, **viewer**, and **terminal**
  surfaces (the terminal attaches to the shared core PTY without owning it). The
  primary window is unchanged. Per-window cost on Linux: a thin detached window is
  ~half a full window's WebKitWebProcess RSS.

## 0.2.9

### Patch Changes

- e1bd064: 0.2.9 — release the 12 commits accumulated since v0.2.8:

  - **AskUserQuestion inline turn** (ADR-011 Phase 3) in chat
  - **Pkg orphan/broken-install detection** with one-click cleanup
  - **DB migrations 0052/0053/0054** — social_queue `media_url` + `hashtags`; atelier wave-4 research + strategy domains
  - **fix:** bind `viewer_port` (not `_viewer_port`) so the release-window URL compiles
  - **fix:** harden the sidecar supervisor against wedged children
  - **ci:** single universal macOS build to cut Actions cost

  No breaking changes; advances the auto-update channel off the v0.2.7 stopgap.

## 0.2.8

### Patch Changes

- Trusted-pkg capability tier (ADR-017) + mutation-worker stack. Signature/provenance-gated elevated capabilities for builtin + signed-registry pkgs: `host.fetch` (mediated proxy with host-side secret injection + SSRF defense), `capabilities.secrets` (named-secret injection), `host.invoke` (scoped command allowlist). Outbound reply-intelligence pulls Twenty CRM live via `host.fetch`, retiring the local mirror. Mutation worker: durable secrets copy for overnight sends, failure surfacing UI, migration `0051`. Install sheet surfaces declared elevated caps + a trust banner; `/settings/pkg-audit` violations view. Fix: release bundle preserves `builtin-pkgs/` per-pkg directory structure (no longer flattened).

## 0.2.7

### Patch Changes

- Heal stale package routes + fix FE SQLite pointing at an empty database. (1) A saved pane at an unregistered pkg subpath (e.g. `/pkg/com.ikenga.tasks/tasks` after tasks moved to a single root route) now redirects to the pkg's primary route instead of a hard "not registered" error. (2) The frontend SQL layer was opening an empty db in the app config dir while all data lives in the app data dir — layout persistence silently fell back to localStorage and "clear local data" silently cleared nothing; both now hit the real database.

## 0.2.6

### Patch Changes

- Grant the updater + process plugin ACL to the main window. The in-app app updater was dead-on-arrival in every prior build — plugin:updater|check was never allowed in capabilities/default.json, so the update check silently failed and About always said "up to date". First build that can self-update via the banner / About page.

## 0.2.5

### Patch Changes

- eb6d578: Fix the pkg update flow: updates are only offered for registry-source installs (builtins update with the shell; dev/local installs are a working tree), one failing pkg no longer silently aborts the rest of the batch, and failures now surface in danger banners on /packages and the auto-updater. Release manifests now include a `linux-x86_64-deb` entry so deb-installed shells can self-update (they previously downloaded the AppImage and rejected it after the progress bar completed).

## 0.2.4

### Patch Changes

- b1777dc: iyke bridge fixes: `/iyke/click` now reports the actual match result instead of a blind `ok:true`, supports click-by-accessible-name, and `/iyke/go` syncs the activity mode to the navigated route.
- b1777dc: Give each app pkg its own activity-bar mode. App pkgs (Suite, Tasks, …) previously borrowed App mode and their published menu clobbered the shell's main nav; now each pkg owns a dynamic `pkg:<id>` mode — its rail icon highlights when active, the sidebar renders the pkg's menu as that mode's body, and App mode (⌘1) always keeps Home/Sessions/Scratchpads/Todos/Cron. Deep links to `/pkg/<id>/…` re-sync the rail; a persisted mode for a since-uninstalled pkg reconciles back to App once the kernel snapshot loads (shell-store persist v13→14, migration preserves pkg modes). The iyke `/iyke/mode` endpoint accepts `pkg:` modes, and its stale Rust validator (which silently rejected `pkgs`/`ngwa`/`artifact-grid`) now mirrors the live core-mode set.
- b1777dc: Full-domain local-store schema gap-fill: embed migrations 0032–0041 (pure-ETL drift fix, `latest_account_balances` view + deterministic id-DESC tie-break, the 14 remaining business tables down-mapped from live Supabase introspection, and `content_performance_history`), bringing the embedded runner to 41 migrations and in line with the canonical ikenga.db. Also hide `visibility: hidden` registry entries (dev/test fixtures + scaffolds) from the default pkg catalog — they stay installable by exact name and keep update detection.

## 0.2.3

### Patch Changes

- Fix the Windows release build failing to compile (E0308 in `screenshot.rs`): the `#[cfg(target_os = "windows")]` window-capture branch passed the `CaptureOutcome` enum straight to `write_capture`, which expects a `CaptureResult`. Unwrap it via the same match the pane path uses (`Ok` → bytes; `Err`/`NativeCrop` → error). Windows-only regression from the 0.2.2 native-crop screenshot change — the macOS/Linux build legs couldn't catch it because the branch is `cfg`-gated, so CI is the only gate.

## 0.2.2

### Patch Changes

- Harden pane/pin screenshot capture so it can no longer freeze or crash the WebKitGTK renderer. Pane capture now prefers a native window-crop (capture the window via the OS tool, crop to the pane's rect with the `image` crate) and only falls back to the synchronous `modern-screenshot` DOM clone when the pane has its own off-screen content — and that fallback is gated by a node-count ceiling that declines cleanly instead of attempting a clone large enough to trip the JSC watchdog. Native-crop validates the captured PNG against the window's outer size before trusting the crop and caches an "unreliable" verdict per compositor (e.g. focus-dependent `gnome-screenshot -w`) so later captures skip the doomed probe. Also: Windows window-capture now falls back to the FE path instead of hard-erroring; the iyke screenshot CLI timeout is raised 15s→70s; and a dropped `log::warn!` in the global-shortcut registration is switched to `tracing::warn!`.
- Slim install size: stop bundling the ~89 MB Bun runtime in release artifacts (deb/AppImage/dmg/nsis) and resolve it at runtime instead (env `IKENGA_BUN_PATH` → version-gated system `bun` ≥ 1.3.14 → cached fetched bun with SHA-pin → post-launch background fetch with a progress chip; sha256-verified before unzip, no-strike park while fetching). Add `[profile.release]` strip + thin LTO so the binary itself is smaller across every target. The app boots and runs without bun; only bun-backed sidecars wait for the background fetch. Offline/air-gapped installs documented (system bun, drop-in binary, `IKENGA_BUN_PATH`).

## 0.2.1

### Patch Changes

- 1b22238: Slim install size: stop bundling the ~89 MB Bun runtime in release artifacts (deb/AppImage/dmg/nsis) and resolve it at runtime instead (env `IKENGA_BUN_PATH` → version-gated system `bun` ≥ 1.3.14 → cached fetched bun with SHA-pin → post-launch background fetch with a progress chip; sha256-verified before unzip, no-strike park while fetching). Add `[profile.release]` strip + thin LTO so the binary itself is smaller across every target. The app boots and runs without bun; only bun-backed sidecars wait for the background fetch. Offline/air-gapped installs documented (system bun, drop-in binary, `IKENGA_BUN_PATH`).
