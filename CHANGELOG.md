# ikenga-desktop

## 0.24.0

### Minor Changes

- 3664127: `provision.sh backups`: Postgres backups as hardened system jobs on a server box — a dedicated `ikenga-backup` user (connection strings from a reserved `backup` secrets scope, GCS service-account key), `pg_dump` 17 from PGDG and `gcloud storage`, per-schedule systemd timers (4-hourly / daily / weekly, UTC), dump → verify → upload to `gs://<bucket>/<db>/<YYYY>/<MM>/`, and a secret-free `status.json` for alerting. Root never follows or writes through a path the backup user controls; disabling removes timers, connection strings and cached credentials.

### Patch Changes

- b35c077: Pkg capability refusals now follow `@ikenga/contract` 0.23.0: every confirmed denial on a `host.*` verb carries `reason: 'scope-denied'`, and a check the shell couldn't run carries `reason: 'check-unavailable'`, so pkgs can map either to its RPC code with `hostRefusalCode()` instead of matching message text. Message text is unchanged.
- 8fa53fc: Permission asks from a terminal on a server now show up where the desktop's do. When a Claude terminal on an Ikenga server holds a tool call for approval, the server records the same notification the desktop does, so the bell, the Companion cards, the home "Waiting on you" tile and Web Push all see it, and answering Allow or Deny from any of them in a browser answers that very request, once. The notification closes itself when the ask is answered, times out (denied), its terminal exits, or the hook gives up, so a dead ask is never left looking pending. On a shared server each person's asks live in their own account: nobody else sees or can answer them, and a project member can only answer asks that belong to the project they were invited to.
  
  Each of those outcomes is also recorded in the access audit log, best effort: who allowed or denied the ask, or that the server denied it on a timeout, or that the terminal ended or the hook gave up with the ask still waiting. A decision that did not take effect is never recorded as one. On a multi-account server each account's process hands its audit records to the server's main process, which writes them; if that process stops before the hand-off, that one record is lost.
- e9e774e: Server provisioning now keeps the agent CLIs current. `provision.sh` installs a daily `ikenga-agent-cli-update.timer` that runs `npm install -g <pkg>@latest` as root for each npm-installed CLI in the profile's `AGENT_CLIS` (claude, codex, opencode, pi). They are installed system-wide, so a person's own `claude` could not self-update and showed "Auto-update failed" in every terminal. Existing servers can add it with `provision.sh install-agent-cli-updates`.
- 994e250: Remote small fixes:
  - Ngwa Health trust card: shows "not available on this server" in the subtitle when trust sources are unserved on the daemon, instead of "unsigned".
  - Ngwa Store: Install and Update buttons in browser remote sessions now honestly state "Not available on this server yet" (via `NOT_AVAILABLE_ON_SERVER_YET`) when actions are unavailable.
  - Terminal hooks: `term_hooks_statusline_snapshot` on a daemon without `--data-dir` reports an honest reason why it is unavailable, which is displayed in CostHud.
  - Artifact comments routing: migration 0071 widens the `artifact_comments.sink` CHECK constraint to allow `'clipboard'` and `'chi'`, preventing routing audit failures after delivery on desktop and daemon.
- 5befed6: Honest failure states: a check that couldn't run no longer shows up as a confident "no".
  
  - Settings › Engines: if your saved custom shells can't be read, Ikenga says so and pauses adding/removing them, instead of showing none and then overwriting the saved list on the next add. If the saved value is corrupt (rather than temporarily unreadable), "Reset custom shells" copies it to a backup setting (`terminal.custom_shell_profiles.corrupt-<time>`), then starts an empty list so you can add shells again; it reads the value again first and leaves it alone if it has been fixed in the meantime. The "Resume terminals on start" setting shows a read error instead of an unchecked box. If your default shell or custom shells can't be read, Settings and the new-tab menu say which shell is being used as a fallback instead of silently opening a different one.
  - Explorer: Todos, Automations and Sessions show a short error row with the reason when their data can't be loaded, instead of an empty state. A package whose manifest can't be read is listed as unreadable, and an unreadable saved terminal list pauses saving (with a workspace banner) until you choose Resume saving, so it isn't overwritten. Resume saving first copies the unreadable list to `terminal.tabs.unreadable-<time>` next to it and tells you where; if that copy can't be made, saving stays paused. If the list reads fine by the time you press Resume saving (the first read only failed briefly), its terminals are restored instead.
  - Chi target picker: when engine detection fails it says "Couldn't check installed engines" with a Retry, instead of "No engine installed". The signed-out copy no longer guesses the cause.
  - Onboarding: an engine whose sign-in check was inconclusive shows "sign-in unknown" with the reason.
  - Pkg iframes: when the shell can't read a pkg's manifest to check a capability, the call is still refused but reports `reason: "check-unavailable"` instead of "pkg lacks the capability". Denials are unchanged.
  - Remote pairing: a temporarily unavailable sign-in check on the computer is its own outcome, not "This browser didn't keep the pairing". If the cookie check gets an unexpected answer (for example a proxy's 502), pairing is still allowed, but the page shows "couldn't confirm the pairing cookie" with the reason and waits for you to click "Open your workspace" instead of continuing on its own.

## 0.23.0

### Minor Changes

- 33d6d31: Multi-user servers (T1):
  
  - **Per-account secrets reach everything an account runs.** The server loads each account's `/etc/ikenga/secrets/<account>.env` into that account's sessions, so its terminals, Chi runs, engine CLIs and package sidecars see its secrets — and only its own. The file must be root-owned and group-owned by the account; dangerous names (`LD_*`, `PATH`, `IKENGA_*`, …) are refused.
  - **`provision.sh sync-accounts`:** shared project downloads (one root-owned, read-only mirror per project; each account gets its own clone on its own branch, sharing objects on disk) and one root-only, scoped secrets file (`everyone` / `agents` / a named account) that hands each account only what it may have.

## 0.22.0

### Minor Changes

- b3c057d: WSL network health. Before a WSL terminal or agent launches, and when a WSL session prints a network error such as `EAI_AGAIN`, Ikenga checks whether WSL can reach the network. The check results are cached for 30 seconds, and nothing polls in the background. When the network is broken, the check names the cause, including a failed mirrored-networking setup (for example `0x8007054f`) read from the Windows event log. The affected terminal pane shows a banner, one notification is raised for each problem episode, and Settings › Engines gains a "WSL health" row. The notification uses a new **system** kind for environment problems, which you can mute on its own from the notification menu. If the problem changes during an episode (for example from no network to WSL not starting), the existing notification is updated in place without a second toast. Each surface offers the fitting fix:
  
  - **Repair DNS** rewrites `/etc/resolv.conf` and backs up the old file first.
  - **Restart WSL networking** opens a single administrator prompt, then shuts WSL down and restarts the Host Network Service.
  - **Switch to NAT** first explains the trade-off for LAN and Tailscale access. It backs up `.wslconfig` with a timestamp, sets `networkingMode=nat` without disturbing other lines, and keeps the file's encoding.
  
  After a fix that restarts WSL, Ikenga reopens the WSL sessions it closed and resumes their Claude conversations.

### Patch Changes

- 3514bd6: Claude terminals on the server now feed the browser's cost HUD and permission inbox. A `claude` launched in a browser terminal is handed the same hook and statusline settings the desktop gives it, wired to the server itself with a secret of its own per terminal (never the server token, and never visible in the process list), so the HUD shows real model, context and cost figures and the permission inbox sees Claude's asks. "Hold PreToolUse" works in the browser: Approve and Deny answer the waiting hook, once, and an unanswered ask is denied after 30 seconds, exactly as on the desktop. A server with no data folder says "Not available on this server" instead of leaving the HUD listening forever. The tool-call feed is not part of this change.
- cf958d5: Faster start in the browser app: icons that pkgs, pins and actions name (for example `layout-dashboard`) now load from one deferred file instead of about 1,700 separate ones. The browser app used to fetch nearly all of them before first paint. Every icon name still works, and the desktop app is unchanged apart from fewer files.
- 1f61833: Onboarding's offline-engine install now names two failures plainly: a registry index whose signature check failed ("couldn't be verified … so nothing was installed"), and a bare browser network failure ("the registry couldn't be reached"), instead of passing through raw error text.
- a4db149: The Update side panel scrolls again: long release notes were clipped at the bottom of the window with no way to reach the rest (or the buttons below them).
- 14b02f2: WSL detection and launches are more accurate on Windows. When WSL can't be asked (it's down, has no distro, or hangs), a Chi run now fails with "WSL unavailable: <reason>" instead of telling you to install a CLI you already have, and agent detection skips that agent for the scan instead of reporting it as missing. A wedged `wsl.exe` no longer hangs detection or run start: every WSL probe has a timeout sized for a cold start. Agents are now probed concurrently. Chi runs, detection and sign-in checks use the WSL distribution set in Settings › Engines, as terminals already did. A project that lives inside WSL (`\\wsl.localhost\…`) no longer fails to start with "The directory name is invalid", and its run starts in the distro that path names. A WSL Codex run no longer guesses a `/mnt/<drive>` path for its working directory. WSL credential checks look only in the home of the user the CLI runs as, in the distro it runs in, so a stale credential in another user's home or another distro no longer counts as signed in. They skip Docker Desktop's distros and report an unreadable or empty WSL share as unknown instead of "not signed in". The terminal menu no longer offers a "WSL (Default)" profile when no distro is installed or `wsl.exe` fails, and it hides Docker Desktop's distros. Seats now check whether an engine is installed on Windows. A WSL that doesn't answer is asked again at most every 30 seconds, so the seat list doesn't stall on it. "Run … login" for a WSL-only engine now runs inside WSL.
- 8281e8f: The onboarding preflight and the Claude store now name the real cause when a check fails. The secrets row tells a damaged secrets index, a locked vault, a vault with no passphrase set up yet, a locked or unreachable keychain, a missing keychain backend and a store disabled at startup apart, each with its own fix, instead of always saying "unlock the keychain". When the free-space check can't match the app-data folder to a volume, it says it couldn't tell (a warning) instead of reporting 0 GB free and failing. Installing from the Claude store with a missing (or non-folder) working directory now says so rather than claiming `git` or `npx` isn't installed.
- 047c95b: When WSL can't be asked, engine detection now says so instead of reporting the engine as missing. The onboarding engine step and Settings › Engines show one "WSL unavailable — <reason>" notice above the engine list (the reason from the first affected engine, with a hint to check that WSL starts), and each affected engine carries only a short dimmed "WSL unavailable" chip; Ngwa's Health › Engines panel and the Companion target picker say "WSL unavailable" too, rather than "Not on PATH" or "not installed". Such an engine can't be picked in onboarding and no longer counts toward "no engine installed". Your default engine stays available in the Companion target picker, tagged "WSL unavailable", so a run on it fails with the WSL reason instead of the engine disappearing; other unchecked engines aren't offered. Detection results gain an optional `unavailable: { kind, reason }` field; it is omitted for every engine that was checked, so older consumers see the same shape as before.
- cbefdc2: Chi runs on an engine that lives inside WSL now check WSL's network before they start. If WSL has no network, can't resolve names, or answers with an error, the run fails at once with "Can't start the engine in WSL — <cause>" instead of launching an engine that can't sign in, and the same WSL health notification the terminal raises appears with its fixes (once per problem, not once per run). The check reuses a result from the last 30 seconds, waits at most 20 seconds, and engines installed on Windows itself are never checked. The run still starts when the check can't tell: when Windows itself is offline, when the check runs out of time, or when `wsl.exe` only timed out (it had just answered the engine lookup).
  
  A persistent Chi run of an engine installed only inside WSL no longer goes to the detached runner, which launches engines from the Windows PATH and can't start it: it runs in-process instead (through WSL, with the same pre-run check) and its result carries the warning "<engine> is installed only inside WSL, which persistent runs don't support yet. This run will NOT survive quitting the app." A persistent run whose engine can't start at all now fails at once with the reason.

## 0.21.3

### Patch Changes

- c0e9418: Live updates now reach the browser. A remote (browser) session used to receive no server events at all, so notifications, the approve gate, settings and the active project only refreshed when something re-fetched them. The server now has an authenticated `/ws/events` channel, and the browser's event listeners use it. The server publishes the same events the desktop app does for settings changes (`settings://changed`), project switches (`projects:active-changed`), actions and keybindings writes and trust changes (`actions://changed`), notifications (`notifications://changed`, with the muted flag), the approve gate (`pa-action-paused` / `-committed` / `-retried` / `-rejected`) and seats (`seats://changed`). Each event goes only to sockets whose access level can read that state. On a multi-user server, each person only receives events about their own workspace. Events the server has no source for, such as the hooks bus, the statusline HUD and runtime downloads, are noted once in the browser console and no longer warn on every subscription.

## 0.21.2

### Patch Changes

- 1daa56e: Browser sessions now get five things that were desktop-only: the title-row git branch chip and the Explorer's git-status badges (`pkg_sidecar_call`), project and personal shell actions (`action_exec`), pin routing to a terminal, Chi or the clipboard (`comment_route`), the schedule table's "Run now" (`agent_ops_run_now`), and pkg settings edits (`pkg_settings_set`). On a multi-user server each of these runs as the signed-in person and stays inside their own files. A sidecar runs only from its own pkg's folder. An action or pin runs only in a folder the server's allowlist covers. "Run now" fires only your own jobs, through your own agent-ops daemon. Settings writes go to your own database, and only for keys the pkg declares. Enabling, uninstalling or restarting a pkg is still desktop-only, because the server has no pkg kernel.
- 2aacb83: Remote daemon: serve Ngwa (`ngwa_snapshot`, `pkg_health_scan`, `pkg_trust_list`). What the daemon cannot evaluate — trust, pkg runtime, usage, install records — reads "Not available on this server" instead of empty or healthy. The Store no longer blames the registry for a snapshot failure, and the Create tab's hardcoded "live" label is gone.
- aff7984: Chi seats now work in a browser connected to `ikenga-server`. The seat rail no longer shows a permanent error, and you can create, rename, clear, remove, hold and release seats. Dispatching to a seat resumes or fills it with a headless Chi run on the server, and a text queued behind a running turn is sent when that turn ends.
  
  Under multi-user (T1) servers, each person sees and changes only their own seats. Things the server can't do are labelled as such rather than failing silently:
  
  - Terminal sessions can't be seated on a server. A seat there runs headless Chi runs instead.
  - `openrouter` seats need the desktop app.
  - An engine missing from the server reads "not installed on this server".
  
  The seat roster also refreshes after your own changes without needing live events.
- e8ff6fd: Browser sessions on a headless server stop offering or claiming what the server cannot do. Native-webview pkgs and the webview "Clear session" control, "Open in browser" / "Copy viewer URL", and the Backup export/restore controls are hidden or read "Not available on this server yet"; HTML, audio and video panes say "Preview not available in the browser yet" instead of a broken localhost frame; package install and update buttons (Store, update sheet, banners, onboarding) are disabled before the click, and the onboarding Done step now counts only installs that actually succeeded and lists the ones that did not (the offline-engine failure names its real cause instead of blaming the registry). The statusline HUD says telemetry isn't available in the browser, Claude-config queries refetch on window focus in place of live watcher events, and "Run now" (Automations), the webview "Clear session" action and the pin composer surface their failures instead of swallowing them.
- e7e8e9e: Browser sessions: the address bar follows the focused pane (in-pane links included), deep links open in the focused pane on load, and a restored terminal no longer steals focus from a deep-linked route.
- 6ba9aa7: Browser sessions can now open folders. On a multi-user (T1) server each person's folder list starts with their home folder: new accounts get it on first sign-in, and existing accounts with an empty list get it too, while a list someone emptied on purpose stays empty. People can add, remove and reset their own folders under Settings → Storage → "Folders you can open", and admins can change anyone's list by username. A single-user (T0) server keeps an empty list until the owner adds a folder, which now also works from the browser. The folder picker no longer falls back to the server's working directory or saves `.` as a project. With no folders it offers "Add a folder", or tells people who to ask when they can't add one themselves.

## 0.21.1

### Patch Changes

- 989b358: Engine detection and Chi runs no longer mistake an infrastructure failure for a verdict. An auth probe that couldn't run (timeout, spawn failure, WSL down, no network) now reports sign-in as unknown instead of "not signed in", so the engine stays in the Chi target picker. A failed run's error names the cause found in the engine's stderr — e.g. "network unreachable from the engine (EAI_AGAIN)" or "WSL failed to start" — without exposing the raw stderr. WSL CLI detection now tolerates login-shell banners printed before `which` output.
- 322ab13: Release build: pin `@codemirror/language` below 6.13.0, whose missing `@codemirror/streamparser` dependency broke the v0.21.0 desktop builds. Same contents as 0.21.0.

## 0.21.0

### Minor Changes

- 6ef1436: Remote server and browser:
  
  - **In-app server updates:** admins see when a new stable server release is available (banner + Settings › Server) and can update from the browser, with the open-terminal count and a health check with rollback (notify-only by default).
  - **Installable app + push:** the browser client is an installable PWA (cached shell, live data) with push notifications for permission requests, finished/failed runs, available updates (admins) and invites/pairing.
  - **Phone dispatch** sends only to agent CLIs or as a Chi follow-up, never into a plain shell.
  - **Clipboard and terminal in the browser:** copy works on plain-HTTP origins or says it couldn't; the paste hint shows the real paste key; the terminal right-click menu no longer closes the moment it opens; Ctrl+Shift+C no longer opens DevTools; OSC 52 copies offer a Copy button when blocked.
  - **Browser parity:** browser-reserved shortcuts get alternatives and file drops are handled; "Open in default app" downloads instead of opening a junk tab; notifications are requested from a click and fall back to a toast (no more phone crash); the app menu shows only what works; pkg links and downloads go through the host; the VS Code keybindings import reads a local file; a shared `toast()` utility.

### Patch Changes

- 05c1bb5: Server releases: `min_upgrade_from` defaults to 0.19.4 (the first release with server tarballs) instead of the release itself, which blocked every `provision.sh upgrade`; `provision.sh` refuses to install an older `VERSION` over a newer server unless `--allow-downgrade` is passed.

## 0.20.1

### Patch Changes

- 505ead6: Windows: resolve extensionless native sidecar bins to their .exe, and resolve `npx` to its .cmd shim for skill installs.
- a7680a0: Fix Windows "not a valid Win32 application" (os error 193) when starting pkg sidecars that declare a .js bin (e.g. Meetings recording): run them through the bundled Bun.
- 1e650ca: Browser sessions on ikenga-server stop the endless failing iyke log/network pushes, pick the server transport before sign-in, no longer call the desktop terminal-attach commands on reattach, and no longer claim "Ikenga is up to date" on Settings › About. Markdown path links no longer raise unhandled errors or resolve relative paths against the server's working directory, and the onboarding footer's "Enter your Obi" now opens the workspace.

## 0.20.0

### Minor Changes

- 756b921: Claude sessions can now be launched with a role, extra system prompt text and plugin folders. A chat or terminal session given the `chi` or `pane` role starts on Claude Sonnet 5.5, and a `plan` session starts on Claude Opus 5.5, unless a model is chosen explicitly. `appendSystemPrompt` is passed to Claude Code as `--append-system-prompt`, and `pluginDirs` reaches it as `CLAUDE_CODE_PLUGIN_DIRS`. Sessions that set none of these launch exactly as before. The model ids and prices come from a copy of the `@ikenga/contract` model catalog. The built-in Claude Code engine's default model setting is now `claude-sonnet-5-5`, and stale model names in the agent editors and Mission Control are updated.
- 4347de2: Claude sessions now launch on the model for their role when no model is chosen. Claude terminals and Claude seats are `pane` sessions and start on Claude Sonnet 5.5. A new "Claude terminal (plan)" entry in the new-tab menu starts a `plan` session on Claude Opus 5.5.
  
  **Behaviour change:** Chi runs on Claude Code (`iyke chi run`, pinned and persistent runs) used to start on Claude Code's own default model. They now start on Claude Sonnet 5.5, the catalog default for Chi. A model passed with `--model` still wins, and other engines are unchanged.

### Patch Changes

- eadc23d: Fix secrets and env-vault writes on macOS: the symlink guard no longer refuses root-owned OS links such as `/var` and `/tmp`, so the env vault under `$TMPDIR` publishes again instead of latching the deny state. User-owned links are still refused.

## 0.19.4

### Patch Changes

- f626f56: Release builds sign and attach the server tarballs.

## 0.19.3

### Patch Changes

- 9159f71: Release builds now attach the server tarballs correctly.

## 0.19.2

### Patch Changes

- e52aafd: Installed apps such as Studio, Tasks and Sales now open in the remote browser client. The server reports app views and rail entries, answers the app trust and badge calls, and lets the browser load an app's own files with a short-lived cookie that only works under `/pkgs` and never contains the server token. App features that run local processes (sidecars, MCP tools, host fetch) report that they aren't available in the browser yet.
- 4ed6dc4: Pin the engine icon library to a release that matches the pinned UI kit, so a fresh install no longer pulls an incompatible newer version and breaks the build.
- 9187a09: Add `secrets rotate-kek` subcommand to rotate the secrets key-encryption key on multi-user servers.
- 4963356: `ikenga-server` gains a `supervise` subcommand for hosts without systemd, such as a container. Run `ikenga-server supervise -- <server flags>` as the container's entrypoint: the server becomes its child and is started again whenever it exits, and `SIGHUP` restarts it on purpose. Detached agent runs are never signalled, so they survive a server crash or restart the way they already do under the systemd unit, and the server picks their status back up when it returns. The supervisor also reaps orphaned processes, so a finished run never lingers as a zombie. `SIGTERM` stops the server and then the supervisor. Stopping the whole container still ends every run inside it. An end-to-end test covers a crash, a requested restart, the reap and a clean stop.
- cd25a7d: Installing from the Store now shows what is happening: the Install button turns into a progress row that names each step (downloading, verifying, extracting, installing dependencies, registering, starting services) with a bar, and you can cancel before the app registers. Several installs, and Update all, each show their own progress. When an install fails you get a short explanation with a next step, such as running out of disk space or a network problem, plus Retry and a "Show details" view with the cleaned-up log and the npm log path. A failed install no longer leaves a half-installed folder behind. Apps that ask to be pinned on install now appear on the rail again.

## 0.19.1

### Patch Changes

- cb55ecd: Apps the registry holds back no longer appear when you browse or search the store. The store was ignoring the registry's hidden flag; it now honours it, and anything you already installed keeps working and still gets updates. The first-run setup also lists only apps that exist in the registry: Tasks is selected by default, Sales is listed but off, and the Files, Mail, Outbound and Content entries are gone. Tasks and Sales are now described as local-only, with the current version and no cloud connector required.
- 35c26a1: People and Secrets now explain that sharing, and your own secrets store, need a multi-user server.
- 34a921d: Clearer names across the app: the rail tips now call Ngwa "your store" and Chi "your agent companion", the Artifact grid is named consistently, the install screen says "Approved" instead of "Trusted", the personal scope reads "Personal" in the Store and Secrets, the first run and Settings say "engine" where they mean the engine, the Gemini filter is gone from the Ngwa engine facet, and the most visible "package" wording now says app or extension.

## 0.19.0

### Minor Changes

- 88024bc: **Phase 7 Part B: people, devices and access.** One Ikenga can now be shared safely, with your own phone or laptop and with the people you work with.
  
  - **Accounts on an Ikenga server (T1).** A server can hold several local accounts. Each person signs in with a username and password, and a broker runs their workspace as that person, never as anyone else. Signing out, changing a password or revoking sessions closes that person's live connections.
  - **Device pairing.** Pair a phone or laptop with a short code (or its QR). Both screens show the same four-word fingerprint, and you confirm on this machine before the device is let in. **Settings → Devices** lists every paired device with where and when it was last seen.
  - **Per-device capabilities.** Each device has its own level (View only, View + dispatch, Dispatch + approve, or Full). Change it or revoke the device at any time; a revoke takes effect at once. A paired device below Full opens a compact remote client: sessions, the permission inbox and a dispatch bar.
  - **Permission routing.** Choose whether a permission ask may be answered on **this device only** or on any paired device, always capped by that device's level. In a shared project, asks that need the Owner go to the Owner, and asks about secrets always do.
  - **Members, roles and invites.** On a T1 server, Owners and Operators invite people with **Share kola**: a single-use, expiring invite that fixes the role (Owner, Operator, Reviewer or Guest) and what is shared. **Members** lists people, roles and pending invites; **Policies** shows what each role can do. On a single machine you remain the only member.
  - **Your secrets stay yours.** On a T1 server each person has their own secret store, sealed under a key the server holds, so one member cannot read another's.
  - **An audit log you can check.** Sign-ins, pairings, grants, revokes, role changes, approvals and dispatches are recorded in a hash-chained log. **Settings → Audit** filters and searches it, verifies the chain, exports it and reseals it after a repair; `ikenga-server audit verify|export|reseal` does the same from the command line.
  - **The People surface (D-05).** Profile, Devices, Members, Policies and Audit share one layout with a Personal / Project scope switch, a visible keyboard focus ring on every control, and light and dark modes.

### Patch Changes

- 5d850f9: Docs: record remote-access Round 16 amendments to the G-PRINCIPAL contract (G-ACCESS shares, pairing, invites).
- 192e522: Move the Tauri command registry out of lib.rs into commands/registry.rs (WP-19 final slice, part A); no behaviour change.
- 052d397: ikenga-server: T1 local accounts groundwork (WP-20 slice 1) — UUIDv7 principal ids and the full `Principal`, the operator root and `operator/accounts.db` (set-keyed migrations, `auth_events`), argon2id passwords with a login verifier and backoff, the provisioning core (uid allocator, useradd or built-in /etc writer, `create_in` guard) and `ikenga-server accounts create|passwd|disable|enable|revoke-sessions|list`.
- 8418bd0: ikenga-server: T1 executor (per-principal setuid spawns with verified drop, env floor, degraded latch), the real T1 boot probe and `ikenga-server probe --executor-tier t1`, and the uid-wide kill on `accounts disable` (WP-20 slice 2).
- a92d573: WP-20 slice 3: the T1 broker — session cookie auth (/auth/*), one setuid ikenga-server child per principal behind an HTTP/WS reverse proxy, and live WebSocket revocation (4401) on logout, password change and CLI epoch bumps.
- 853dd4d: WP-20 slice 4: T1 browser sign-in (username/password, cookie-only transport), `ikenga-server accounts adopt-t0` to migrate a T0 install into a principal, and a T1 systemd unit that keeps detached chi-runners alive across restarts.
- 44a8e33: Per-principal secret store for T1 (WP-21): each principal's own secrets, sealed under a key the broker derives from a root-held KEK (`operator/secrets-kek`, back it up), layered over the IKENGA_SECRET_* operator default. T0 is unchanged, including its secrets lock-state answers, so Settings → Secrets stays read-only there. A lost KEK is never re-minted over existing stores: the launch fails with "restore operator/secrets-kek from backup". A principal's secret values are capped at 64 KiB each and 8 MiB per store; a write past either is refused with nothing written.
- f770245: G-ACCESS skeleton (WP-74a): capability vocabulary + generated types, the access store and its migration set, device records with revoke/tier-change socket closing, the hash-chained audit core, the T1 broker hook fills, and every Part B command registered skeleton-first.
- 46a3cd6: G-ACCESS WP-74b: device pairing and the remote client. A short code (10-minute expiry, QR included) drives a SPAKE2 handshake (Rust `spake2 =0.4.0` on the host, a `@noble/curves` port in the browser, shared test vectors); both sides show a 4-word fingerprint phrase (EFF Large Wordlist) and the host confirms on a full-window `pair-confirm` before a per-device grant is issued as an HttpOnly cookie. Attempts are throttled per address and per host. Settings › Devices lists paired devices with capability tiers and immediate revoke; a paired device below `full` boots into the 390 px `/remote` client (sessions, permission inbox, dispatch bar); the re-auth overlay gains "Pair this device".
- e9c1f50: Remote permission routing (WP-75, G-ACCESS §5): the per-principal "this device only" / "any paired device" preference capped by each device's grant, the sensitive-ask classifier and the §5.4 who-may-decide algorithm (Owner escalation in shares, secret-material asks always to the Owner, members never persist rules), one `permission_decide` core for hook and ACP asks (desktop in-process, daemon over the relay), the T0 desktop → daemon ask relay so a paired device can answer the desktop's asks, attribution on permission rows, the remote inbox's read-only states, and the Ngwa trust sheet's operator-policy split on T1.
- 706f8b2: G-ACCESS WP-76: members, roles, invites ("Share kola") and sharing on a T1 server. Owners and Operators issue single-use, expiring invites with role and scope fixed at issue; accepting one adds the membership — or, only when the issuer is an admin (or the server runs with `--member-invites-create-accounts`), creates the account through the provisioning core in the same transaction. The broker routes a share request into the Owner's workspace capped by role ∩ device tier; the Owner's workspace confines it to the project (or one artifact), filters lists, strips cost figures for Reviewers and never injects vault secrets. Settings › People gains Members (people, roles, Remove with Undo, pending invites, "Shared with you") and Policies (the role matrix backed by data, "Require Owner approval", spend cap shown but not enforced). On a T1 server the sign-in screen is restyled with "Pair this device with a code", Profile gains the Account block (username, principal id, change password, sign out), and Settings › Secrets shows whose vault it is. The desktop keeps its single-owner view.
- 3fdafb0: G-ACCESS audit log (WP-77): the Audit tab (filters, search, export, degraded banner and reseal), `access_audit_list` / `_verify` / `_export` / `_record_local` / `_reseal`, the reseal-aware boot verify, `auth_events` absorbed into the chain (`access/0002`), `dispatch.sent` on remote Prompt/Write frames, app-lock and vault rows from the desktop, and `ikenga-server audit verify|export|reseal`.
- ef7b3df: Part B follow-ups (WP-78a): the T0 daemon's permission routing shares the server's one `ikenga.db` writer; refused hook decisions are shown, never presented as answered, and the no-row hook fallback fails closed without a routing runtime; a desktop restarted after a crash reports its orphaned asks so the daemon retracts an unapplied `permission.decided`; the T1 broker audits `routing_refused` decisions; the notification popover shows why a routed-away ask has no Allow / Deny; `--max-accounts` caps every account-creation path (CLI and bootstrap included); `secrets_default_names` tells your own bare keys from operator-default overrides; `share.artifact_viewed` is written for Guest and artifact-scope reads; WebSocket closes 4401 / 4403 route to sign-in and "access changed"; and secrets-declaring pkgs' MCP servers feed the sensitive-ask classifier.
- 1eec301: People, devices and access (D-05) design sweep: D-05's type scale and muted sub-lines now apply (the size utilities were compiling to colours), every control shows a 2px solid focus outline, light-mode button edges use the border token, danger buttons and the capability / role pills match the design, the pair confirm takes and keeps keyboard focus, People's header drops "Open file" and the iyke line, and a paired phone below Full gets a working transport in `/remote`.
- c4eea63: People, devices and access: the keyboard focus ring on the Personal / Project scope switch is no longer cut off by the switch's rounded edge; it now shows in full, in light and dark mode.

## 0.18.6

### Patch Changes

- 9c7f0ae: Clicking a package row in the Explorer's "Ngwa · project" section now opens that package's Ngwa item detail page instead of a package UI route — fixing a "No such package route" error for engines, skills, and MCP-only packages that have no UI of their own.
- 3f7436d: The Explorer's "Ngwa · project" section now lists the active project's full mix of Ngwa items (skills, agents, hooks, MCP servers, workflows and pkgs) from the Ngwa snapshot, each with a kind icon and label, instead of only kernel pkg rows — matching locked design D-01. It falls back to the previous kernel-pkg list while the snapshot's cold scan is still running, and its Explorer row-count badge now hides at zero like the other sections.
- e2dadd5: Pane tabs for an installed pkg or an ngwa item (skill/agent) now show their real name ("Studio") instead of the title-cased pkg id ("Com.Ikenga.Studio") — the same fix applies to the single-tab address bar and the ⌘K switcher.
- 8cc0c8f: Package installs no longer leave scratch files behind in the pkgs folder. Each registry install now names its downloaded tarball after the full package id (previously every `com.ikenga.*` install shared one `.staging-com.ikenga.tgz`), and on startup the shell cleans up staging folders, tarballs and backups left by an install that was interrupted by a crash or restart, restoring the previous version of a package if the update died before the new one was put in place.
- 6b38a4d: Packages bound to the Default project now count as personal, so they stay loaded whichever project is active. Before this, every package on an existing install was stamped with the Default project on each start, and switching to any other project parked all of them, built-in packages included. A migration clears the existing Default stamps, the start-up backfill no longer re-stamps packages, and installs or scope changes that target the Default project are stored as personal. With Default active, the Store's install button now reads "Install to personal", and Default no longer appears as a separate install target.
- 7c17836: An installed package's recorded version now stays in step with the version the shell shows. Hot-reloading a dev package, or starting the shell after a package's manifest changed on disk, now updates the stored install record too, so it no longer reports an old version (for example 0.6.0 while the Explorer shows 0.8.0).
- aa79e85: The status bar's middle Ngwa group now labels its install count "N pkgs" (singular "1 pkg") instead of "N installed", because it only ever counted the pkg kernel's installed rows, not the full Ngwa catalogue (skills, agents, hooks, mcp tools, …) the label implied. The Ngwa Installed tab now shows a small muted "· N pkgs" sub-count next to its total, computed by the same shared `selectPkgCount` definition, so the two numbers always agree.

## 0.18.5

### Patch Changes

- 6a18239: Fix the Ngwa **Installed** tab being stuck on "Scanning equipment catalogue…" forever. Building the snapshot called `Kernel::status()` from async code. The settings registry's `snapshot()` reads `pkg_settings` with a blocking `block_on`, so as soon as any installed pkg declared a settings schema (e.g. Meetings 0.2.1), every snapshot panicked with "Cannot start a runtime from within a runtime". The panic affected both the `ngwa_snapshot` command and `GET /iyke/ngwa/snapshot`.
  
  - Both callers now take the kernel status on the blocking pool.
  - The settings registry reads values safely from any context: no runtime, a multi-thread runtime (`block_in_place`), or a current-thread runtime, where it degrades to schema-only instead of panicking.

## 0.18.4

### Patch Changes

- 0995128: Uninstalling or reinstalling a pkg now stops its running processes first.
  
  - Uninstall stops the pkg's long-lived MCP server or supervised sidecar and waits for it to exit before it moves the pkg's folder. The wait is bounded. A process that ignores the stop is killed; on Windows its whole process tree is killed. Before this fix, a running process could keep the folder locked, so the uninstall only half-completed and a reinstall then failed with "os error 32".
  - Reinstalling from the registry stops the running copy of the pkg before replacing its folder. The folder move retries briefly. If the folder is still locked, the error now names the holder, for example "a Meetings process is still running (pid N)".
  - A pkg that is parked or uninstalled while its MCP server is still starting is now stopped. Before, it was marked running and left orphaned.
  - Project reconcile never parks a workspace-scoped pkg. It also no longer re-registers a pkg that was freshly installed and is already running.

## 0.18.3

### Patch Changes

- e35b1cf: Ngwa Health now matches its D-02 design, and removing broken pkgs works.
  
  - The six panels (Violations, Sidecars, Cron, Data, Trust, Engines) grow to fit their rows, and the page scrolls. Panels no longer clip their rows or hide buttons such as "Last backup" and "Reinstall from registry".
  - Violation rows carry their actions inline. A pkg in the pkgs folder that failed to load offers Reinstall from registry (when the registry lists it), Remove… and Hand to Chi.
  - Trust is its own panel.
  - Remove on a folder that failed to load moves it to a recoverable `.uninstalled-…` backup, as uninstall does, instead of deleting it.
  - Remove all covers everything the scan lists, folders included. It rescans afterwards and says exactly what it removed and what is left, and why. It no longer reports "done" while an issue remains.

## 0.18.2

### Patch Changes

- 6004afa: A pkg that is on disk but fails to register at boot (for example a manifest still on `ui.nav`) is no longer invisible. Its rail pins are hidden (not deleted) until it registers again, so the rail never shows a dead icon. Ngwa Health now lists it with the parse error, from `pkg_health_scan`'s new `pkgs_dir_unloadable` and `register_failed` kinds, and offers "Reinstall from registry" (or Remove, which deletes the unloadable folder). The Store row reads "installed · failed to load" with a Reinstall action that goes through the normal consent sheet.
- 23adc9c: Uninstalling a registry or CLI-installed pkg no longer brings it back after a restart. The kernel now moves the pkg's folder under the app's `pkgs` dir to a hidden `.uninstalled-<id>-<time>` backup (kept 7 days, pruned at boot), so boot discovery no longer re-registers it as a local install. Builtin, dev, and out-of-tree local installs are never touched.

## 0.18.1

### Patch Changes

- 4a1ca82: Ngwa Store: an update that asks for new permissions is no longer a dead end. "Update" and "Update all" in the Store, and Update in the Installed tab (detail and context menu), now open the updater's capability review instead of failing — Approve installs that version, Reject skips it. The updates strip reads "N need approval · Review", with real failures shown apart. The Store's install sheets also write out why Install is disabled, with a live consent count ("Tick every consent above first (2 of 6 ticked)").

## 0.18.0

### Minor Changes

- 066c821: **The Store installs git and npx primitives, pinned to what you reviewed (D-02 addendum, Round 57).**
  
  - **Signed-catalog rows in the Store.** The curated `primitives.json` entries list beside the registry pkgs, with **Source** chips (registry · git · npx) and a `hook` kind. When a catalog entry and a registry pkg share a kind and name, one row shows: the registry row, with an `also: npx` note. A catalog row's sheet shows the **requires** closure before consent, one Share-kola box per dependency that isn't in the catalog, the trust facts, and **Install to \<project\> ▾**.
  - **Add from URL…** beside the search box (and from an empty search, carrying the query): paste a git URL or an `owner/repo` spec, **Resolve** to see the kind, commit, files and closure *before* anything is written, acknowledge the unsigned source, then install — exactly the commit that was resolved.
  - **Pinned installs.** Catalog entries can now carry a pinned commit (`ref`) and content `hash`. Catalog installs follow the catalog's pin instead of HEAD, and join the Updates strip when the pin moves. An install or update that no longer matches the reviewed commit or content is refused, and nothing is written.
  - **Installed: Update and Remove… for git/npx items.** Update shows `<installed> → <remote>` and confirms before fetching exactly that commit. Remove… is dependents-aware: it lists every link and every item that requires it, then lets you unlink and delete, relink to another copy and delete, or forget the record and keep every file (the item then shows as `local`).
  - **Hooks and MCP servers install from git** (as settings fragments), from the catalog or a URL.
  - Backend: new `oba_resolve_source` dry-run; `oba_install_git` / `oba_install_npx` / `oba_install_with_deps` / `oba_update` accept `expectSha` / `expectHash`; `oba_auto_update_all` takes the catalog pins; git runs with credential prompts disabled (public sources only).

## 0.17.0

### Minor Changes

- 1e2452c: **The Ngwa Store works, and matches its design (D-02).**
  
  - **Layout:** the install sheet no longer squeezes the result list away (#324), and the **Install ▾** scope menu is a proper popover that closes on Escape or a click outside.
  - **Kinds are right** (#325). The `@ikenga/mcp-*` servers show as **tool**, and embedded and windowed pkgs as **app**. Installed pkgs use the kernel's own kind, so every registry entry now lands under a Kind chip.
  - **Full install sheet** (#326):
    - an overview;
    - the **requires closure** you are about to pull in;
    - **permissions** with one consent checkbox per group; Install stays disabled until every box is ticked;
    - trust and provenance;
    - a sticky "Install to <project>" foot.
  
    Rows show what each pkg pulls in and asks for, and the manifest is fetched only for the row you open.
  - **Install and Update actually run.** The Store now installs through the same signed-plan registry path as the other install surfaces:
    - one install per closure step;
    - **personal** maps to the workspace scope, **project** to the active project;
    - Update installs the latest version in the pkg's current scope, and holds back if the new version asks for more permissions;
    - lists refresh afterwards.
  - **Installed and item-detail actions work** (#328, D-02 / D-08).
    - **Installed tab:** the detail column and a new row context menu run Disable/Enable, Move…, Copy to…, Update, Open folder, **Remove…** (with confirm) and Hand to Chi.
      - Remove uninstalls a pkg, or removes a skill/agent/command/hook from its scope.
      - Builtin pkgs can't be removed.
    - **Item-detail page:** Open view, Disable/Enable and the ⋯ menu (Open manifest.json, Reveal install path, Reset settings to defaults, Copy as iyke).
    - **Shared code:** Scopes, Installed and item detail now share one action set and one set of confirm dialogs.
    - **Rail:** icons pinned for a pkg are pruned when it is uninstalled.

## 0.16.1

### Patch Changes

- bbfdce6: Pin the `@tauri-apps/plugin-*` npm packages to the minor versions of their Rust crates. The v0.16.0 release build failed on all three platforms with Tauri's version-mismatch check, because the release installs without a frozen lockfile and the `^` ranges resolved to newly published 2.4/2.5/2.8 plugins against 2.3/2.4/2.7 crates.

## 0.16.0

### Minor Changes

- dc77f24: **Phase 7 Part A: Chi seats, and local app lock.** A **seat** is a named, per-project slot for an agent session: `seat:<project>/<name>`, for example `seat:royalti-co/lead`. You dispatch to the seat, not to whichever session happens to hold it. The seat keeps its name, its scratchpad and its address when that session ends.
  
  - **The seat rail (D-09).** The Companion lists the active project's seats, followed by an "Unseated" group.
    - Selecting a seat targets dispatch to it and scopes the permission, cost and tool-feed panels to it.
    - Each row shows the seat's status (live, idle, running or vacant), its session, a scratchpad line and where it is mounted.
    - Figures an engine doesn't report read "—", with a "not reported by this engine yet" tooltip.
    - **New seat** makes an empty seat, or seats an open or past session. **Remove seat…** asks first and can be undone. **Clear** empties the seat and keeps its scratchpad.
  - **Dispatch to a vacant seat** resumes its last session, then sends, and a toast says so. On an engine that can't resume after a restart (openrouter), the seat is marked "not resumable after restart"; it never silently starts fresh.
  - **Moving a session into a seat** takes it out of any other seat, atomically.
  - **Holds.** A seat held by another client shows "held by X since T". Taking it over is always an explicit step.
  - **`/chi` is the seat board.** It is a table of every seat plus the unseated sessions, with a detail column and the rail's own menus. ⌘2 still opens the rail. ⌘2 pressed from the dispatch input opens the board (`chi.board`).
  - **Pop out joins Window 2.** A seat's or a pane's Pop out puts it into the open secondary window as a tab. It opens a new window only when none exists. The seat stays addressable while it is out there. Closing Window 2 brings its panes back to the main window.
  - **Actions.** A `chi` action can target a seat by name. The Editor has a seat field, and the runner's liveness, permission and trust guards still apply.
  - **Settings → Profile, App lock and Devices (D-05, local).**
    - The app locks on idle or with Lock now (⌘⇧L), and unlocks with a PIN or passphrase.
    - The lock screen says what is still running underneath.
    - Devices shows the daemon's current exposure, read-only, and says plainly that it is not a security boundary.
  - **Light mode.** The cost HUD, tool feed and permission inbox use theme colours instead of fixed dark greys.
  - **`iyke`.** The bridge is at API level 5, with seat routes. The `iyke seat ls | create | resume | fill | clear | release` commands and `iyke terminal-send --seat` ship in iyke-cli v0.6.0.

## 0.15.1

### Patch Changes

- 3db56cc: Pin `@tauri-apps/api` to `~2.11.0` to match the `tauri` crate (2.11.x, locked in `Cargo.lock`). `@tauri-apps/api` 2.12.0 was published to npm on 2026-09-26. The release workflow installs without a frozen lockfile, so `^2.1.1` picked 2.12.0, Tauri's version check refused the mismatch, and the v0.15.0 build failed on every platform. v0.15.1 ships Phase 6.

## 0.15.0

### Minor Changes

- 42205c9: **Phase 6: actions, menus and keys.** Every menu item, key and shortcut in the shell is now data that you can see, rebind, reorder and extend. It lives in two files at two scopes: `~/.ikenga/{actions,keybindings}.json` for personal, and `<project>/.ikenga/…` for project.
  
  - **One key dispatcher.** The registry is the single place keys fire from. It covers the workspace, the rail, the palette, zoom, the terminal chord table and the leftover widget handlers.
    - Chords wait up to 900 ms, and only when a chord starting with ⌘K is actually bound; otherwise ⌘K opens the palette immediately.
    - Typing and IME never fire a shortcut.
    - The macOS menu-bar accelerators are deduplicated, so a key fires once.
    - **Windows/Linux:** Alt+1–6 now focuses pane N. Ctrl+1–3 still switches the rail.
  - **OS-wide shortcuts.** Summon, window screenshot and pane screenshot are ordinary keymap rows with `scope: 'os'`. You can rebind them in your personal file. A project or package can never claim an OS-wide key.
  - **Menus render from data.** The Explorer, tab, pane `⋯`, status bar, rail, palette groups and the native menu (76 items across 8 files) all come from the effective model.
    - Reorder or hide an item in `actions.json` and it applies live, with no restart. Locked items can move but never hide.
    - Package actions appear where their manifest places them.
  - **Your own actions.** There are six run kinds: `chi`, `shell`, `iyke`, `skill`, `workflow` and `open`, plus built-in and package kinds.
    - **Trust in a project.** In an untrusted project, `shell`, `iyke`, `skill` and `workflow` actions refuse until you trust them in the trust sheet, and editing the command asks again. An untrusted project's keybindings are "held until trusted".
    - **Test run** previews rather than acts. For `shell`, and for `iyke` actions that send a POST, it shows what would run instead of running it.
  - **Settings → Actions, menus and keys (D-06).** It has five parts: the Actions list, Editor, Menus, Keys (with conflicts), and Import. Import takes a package, the core of your VS Code `keybindings.json`, or a teammate's project file. An import never overrides a binding you already have.
  - **`iyke`.** The bridge is now at API level 4, with actions, menus and keys routes. The matching commands (`iyke actions`, `iyke menus` and `iyke keys`) ship in iyke-cli v0.5.0.
  - **Packages.** A manifest can request a single-stroke `key` on `ui.context_actions[]` (G-PKG-KEY). It is granted only if the key is free. `ui.command_palette[].action` is now typed.
  - CI now builds against `@ikenga/contract` HEAD again. The pin added during Phase 6 has been removed.

## 0.14.1

### Patch Changes

- df6d123: **Duplicate and blank surfaces from the 0.14 re-architecture.**
  
  - **Settings** lists its nine sections once. The nav sits inside the pane (D-03), and the sidebar stays the Explorer.
  - **Chi and Ngwa sidebars are no longer blank.** Chi shows Sessions and Ngwa shows the project's equipment (D-01). Clicking Ngwa now lands on `/ngwa/installed` instead of `/claude`.
  - **One project switcher.** The rail-foot copy is gone; the title-row chip does the job.
  - **One greeting on the Project dashboard.** The Obi canvas hides its own greeting while the daily address shows.
  - **One toast per permission ask.** The terminal permission inbox no longer fires its own OS notification for asks the notification centre already records.
  - **The Ngwa Store uses the shared D-07 states.** Loading shows the ember pulse, and the offline state has a Retry. Two unmounted legacy components (`PkgsSurface`, the old Ngwa facet bar) were deleted.

## 0.14.0

### Minor Changes

- a9cd840: **Phase 5b: flows, viewer and pane chrome.**
  
  - **Onboarding (D-04).** Consecration is rebuilt as seven steps: welcome, engine, project, equipment, look, shortcuts, done. It has a step rail, resume/offline/engine-none states, and a Personal/Project scope switch. It never scaffolds into `~/.claude/` implicitly.
  - **Daily address.** It sits on the Project dashboard. It shows project-scoped runs and unresolved permissions, is dismissible once a day, and has a real Workspace setting.
  - **Notifications.** A notifications table (`shell_notifications`, migration 0066) records permission, run, update and violation rows. Rows keep a resolved state that is separate from read. Terminal prompts resolve on `Stop`, `PostToolUseFailure` and the next `UserPromptSubmit`. Per-kind mutes live in `settings.json`. The status-bar bell and a notification centre show the rows, toasts are text-only copies of them, and the iyke bridge serves `GET /iyke/notifications`.
  - **Updater (D-07).** One updater flow covers the app and packages: release notes, status-bar progress, restart with a live-session warning, and a package batch that holds any update requesting a new permission for review *before* installing it.
  - **Automations and restore (D-07).** There is a `/automations` view. A restore wizard covers the whole restore, from picking the file to done, and lists secret names only. Shared Empty, Loading, Error and Offline states each offer exactly one next action.
  - **Viewer and panes (D-08).** The artifact viewer gets new chrome, variants, and CSV and JSON renderers. Package panes get states for loading, consent, crashed, sidecar-down and blocked, with a working "Allow host…". Native menu parity: the macOS tree, plus a `≡` cascade on Windows/Linux, with keys taken from the keymap registry.
  - **New commands:** `notifications_*`, `pkg_trust_preview_incoming` and `secrets_index_names`. Each has its ACL entry.

## 0.13.0

### Minor Changes

- f86a32f: **Phase 5a: settings shell + secrets unlock.** Settings is rebuilt on the
  `urn:ikenga:settings:v1` contract (`~/.ikenga/settings.json` personal +
  `<root>/.ikenga/settings.json` project, KV migration with durable markers),
  collapsing 15 legacy routes into the D-03 nine-section shell with
  Personal/Project scope switching, cross-section search, project-override
  markers with revert, Open file / Copy as iyke, and reset-section. Legacy
  routes (`/settings/activity-bar`, `agent`, `artifact-grid`, `backup`,
  `onboarding`, `packages`, `pkg-audit`, `pkg-health`, `data-health`,
  `terminal`) redirect. Secrets gain a passphrase-gated encrypted layer
  (Argon2id + AES-256-GCM) with set/rotate/unlock/lock commands, idle
  re-lock, a global unlock sheet, Linux Secret Service as the authoritative
  backend with keyutils as a read-only fallback, hardened Stronghold
  migration rollback, and redacted-only reveal in the UI. New commands:
  `secrets_set_passphrase`, `secrets_unlock`, `secrets_lock`,
  `secrets_lock_state`; `secrets_vault_status` now reports
  `locked/configured/idle_timeout_secs/last_activity_unix_ms`; the iyke
  bridge gains `GET /iyke/secrets/lock-state`.
- 8504409: **BREAKING: `ui.nav` removed (DEC-37 cutover).** The `ui.nav` → `ui.views`
  alias had a one-release lifetime (G-MANIFEST-V5 §4) and v0.12.0 was the
  soft-warn release, so the Rust manifest parser now rejects `ui.nav` outright
  with a canonical message naming `ui.views[]`. `Manifest::apply_nav_views_alias`,
  `NavAliasOutcome` and `normalize_nav_route` are gone; `Package::load` does no
  post-parse fixup. The `NavEntry` wire shape survives on the activity-bar
  registry snapshot (pkg-mode sidebar + WP-22 pin seed read it), sourced from
  `ui.views[]`.

## 0.12.0

### Minor Changes

- 476f7df: Phase 4 — manifest v5 contributions and workflows (soft-warn release). The kernel accepts `ui.views[]`, `ui.explorer_sections[]`, `ui.companion_panels[]`, `ui.context_actions[]`, `ui.widgets[]` and top-level `workflows[]` (`@ikenga/contract@0.20.0`); `ui.nav` still parses with one warning per package and is removed in the next release (DEC-37). Adds `pin_on_install`, the `GET /iyke/ngwa/snapshot` and `GET /iyke/explorer/sections` bridge routes (`BRIDGE_API` 3), workflow importers (groundwork, Claude Code workflows, hooks, cron) with a non-executing parser, the Ngwa flow tab and Explorer Automations listing.

## 0.11.0

### Minor Changes

- ae96cff: Phase 1 of the shell UX rearchitecture: the workspace is now organised around three nouns — Project, Chi and Ngwa.
  
  - **Rail** is exactly Project · Chi · Ngwa · pins · Settings. The old per-pkg activity modes are gone; a v15 persisted store migrates to v16 (unknown modes snap to `project`, file roots fold into the active project, and the pre-migration blob is kept under a `__v15_backup` key). Rail pins are seeded once from the kernel activity-bar registry at upgrade.
  - **Project** gets an Explorer sidebar scoped to the active project, with a section registry and eight built-in sections (files, artifacts, sessions, ngwa-project, automations, todos, scratchpads, views), single-action empty states and a project switcher. Location collapses onto the active project rather than being tracked separately.
  - **Chi** gets a Companion panel: dispatch bar with target picker, session tabs, permission cards, cost HUD, tool feed and a collapsed resting strip. Dispatching with no terminal focused starts a headless `chi_run`.
  - **Ngwa** gets a single `/ngwa/*` route family with an in-route facet bar (kind, system, scope) instead of rail entries. Legacy routes redirect.
  - **Frame chrome**: a title row with exactly two controls, one banner slot, a status bar, a merged single-tab pane row with a tools reveal, and a Shortcuts view generated from a new read-only keymap registry.
  - **Bridge**: `iyke state` reports `active_project` and `bridge_api`, and `GET /iyke/keys` exposes the keymap.
  
  Verified against the locked D-01 design: 32 resting interactive controls at the default layout against a budget of 132, WCAG 2.5.7 / 2.5.8 / 1.4.11 / 1.4.13 checks green in the browser-mode e2e harness.

### Patch Changes

- 29859dc: The Chi Companion's target picker is now a well-formed menu. Its options were each wrapped in a bare `<div>`, so `role="menu"` owned no `menuitem*` or `group` children (an `aria-required-children` violation that lets an accessibility tree flatten the options into text); same-group options now sit inside one `role="group"` that carries the caption as its accessible name. The chip also follows the APG menu-button pattern: ArrowDown opens it on the first option, ArrowUp on the last, and Home / End move to either end of the open menu.
- ae96cff: Fix two Windows-only papercuts. Console-subsystem children (node, npm.cmd, wsl.exe, taskkill, tmux, …) spawned from the GUI process no longer flash a visible console window — every production spawn site now goes through a shared `NoConsoleWindow::no_console_window()` helper in `platform.rs` instead of each callsite re-deriving `CREATE_NO_WINDOW`, including `terminal::multiplexer`'s tmux probes. (`pty::daemon_client`'s detached `ikenga-server.exe` launch — the most visible offender, a console that stayed open for the daemon's whole lifetime — is covered separately in #201.) Separately, three timeouts tuned on macOS/Linux were too tight for a Windows cold start (Defender scanning a freshly-spawned binary on first exec can add 500ms-1.7s+): the MCP per-call handshake budget is now split from the `tools/call` budget (15s to spawn+initialize, 5s for the call itself), the long-lived sidecar `INIT_TIMEOUT` moved from 5s to 15s, and the agent `--version` probe timeout moved from 2s to 5s.
- ae96cff: Windows home-directory resolution. Over a dozen production call sites read raw `$HOME` — unset on a normal Windows GUI launch — instead of the existing `platform::home_dir()` helper (which falls back to `%USERPROFILE%`/`%HOMEDRIVE%%HOMEPATH%`), so Claude session discovery, agent-ops, backups, the claude-store hook/MCP/skill installer, PATH augmentation, and engine default-cwd resolution all silently failed or fell back to `/` on Windows. `screenshot_cli_control_path()` also had an early `HOME`-read `?` that returned `None` before ever reaching the Windows branch, which doesn't need `HOME` at all — restructured so the Windows branch resolves independently. (The equivalent early-return bug in `log_dir()` is fixed in #201, and the durable env-vault path in `commands/secrets.rs` now uses `%LOCALAPPDATA%\ikenga-actions` per #202 — both out of scope here to avoid three PRs fighting over the same lines.) `npx skills add`'s sandboxed install now also pins `USERPROFILE` alongside `HOME`, since Node's `os.homedir()` reads `USERPROFILE` on Windows and the previous sandbox was a no-op there. Test helpers that fake `HOME` now set/restore `USERPROFILE` too, so they stay hermetic under the new resolver.
- ae96cff: Stop writing vault env files to a world-readable folder on Windows. The runtime `env-vault` and the durable `env` copy used to fall back to `/tmp`, which resolves to `C:\tmp\ikenga-actions` where any local user can read them; they now live in `%LOCALAPPDATA%\ikenga-actions`, inside the user's own profile. Copies left in `C:\tmp\ikenga-actions` by older builds are deleted once per run, before the vault is opened, so the cleanup happens even if the vault fails to open. The cleanup only removes regular files and skips symlinks and junctions, so another local user cannot redirect it into your profile. macOS and Linux paths are unchanged.
- ae96cff: `cargo test` works on Windows again. Test binaries were linked without the Common Controls v6 manifest that tauri-build only gives the app binary, so they loaded System32's comctl32 5.82 (no `TaskDialogIndirect`) and died with 0xc0000139 before running a single test. The manifest is now passed to the linker for every artifact on Windows MSVC targets; other targets are unchanged.
- ae96cff: Windows terminal fixes. Copy/paste goes through the Tauri clipboard plugin instead of `navigator.clipboard` (no more WebView2 clipboard prompt; silent paste failure when it was dismissed), plain Ctrl+C with a selection copies and Ctrl+V pastes, and OSC 52 writes (claude's own copy) reach the system clipboard. Links that wrap across rows are clickable again under ConPTY. The PTY daemon now gets 3s to come up instead of 500ms (every cold start on Windows missed it), spawns without a console window, and a daemon that misses the window is killed rather than orphaned on port 4000. The log file is written to `%LOCALAPPDATA%\Ikenga\logs` again — `log_dir()` bailed on the unset `HOME` before reaching the Windows branch.

## 0.10.3

### Patch Changes

- 6041772: Add the `host.sendToActiveSession` AppBridge verb (#127). Stop holding the kernel `live` lock across registry calls in `reconcile_for_project` so a registry panic can no longer poison it (#131). Write `cwd` for pkg MCP servers in `~/.claude.json` so relative args resolve (#128). Version PRs now open with `WORKSPACE_DEPS_PAT` so their CI isn't approval-gated (#193).

## 0.10.2

### Patch Changes

- 756d5ce: Fix tab drag-to-split, tab reorder, dock drags and pin reordering on Windows. WebView2 blocks HTML5 drag events while Tauri's native file-drop handler is enabled, so these drags now run on pointer events instead; dropping files from the OS onto terminals keeps working on every platform.
- 050d17b: Open each terminal tab's PTY exactly once. Restored and newly opened tabs used to spawn several PTYs (the rehydrate auto-resume and every re-run of the tab's mount effect each started one) and kill all but the last within milliseconds; PTY opens are now single-flight per tab, and a tab closed mid-spawn no longer leaves an orphan process.

## 0.10.1

### Patch Changes

- aa5f2bb: Fix launching Claude terminals on Windows when `claude` lives only in WSL: the `--settings` path is now translated to `/mnt/<drive>/…`, and `wsl.exe -e` stops the distro login shell from expanding the wrapper's `$__status` early.

## 0.10.0

### Minor Changes

- f001bfb: Terminal overhaul (Waves 1-4), OpenRouter engine, and pkg-trust relay.
  
  - Terminal: desktop PtyManager daemon proxy and session continuity (WP-01, WP-02); deep linking and navigation (WP-05, WP-07); OSC 133 semantic prompts (WP-08); live foreground CWD, rich search options and renderer hygiene (WP-182, WP-183, WP-185).
  - Engines: OpenRouter HTTP engine — the shell half of WP-20.
  - Iyke: pkg-trust relay endpoint for studio project access (WP-04).
  - Manifest parser: accept a boolean `sqlite` capability and a `//` comment field (#179); accept an optional `description`.
  - ACL: grant `pty_daemon_info` and `pty_daemon_shutdown` in allow-app-commands.
  - Scripts and tests: unbreak two dead royalti-video-engine references; cover migration 0063's meetings schema and reader-pool visibility.

## 0.9.0

### Minor Changes

- ce7efaf: feat(webview): pane-chrome session controls + a real origin boundary (PR #163)
  feat(manifest): mirror v4 schema in the Rust parser — session, events, auth_bridge, route partition, allowed_origins (PR #162)
  fix(shell): safe import.meta.env, createRoot imports & official Git branch icon (PR #161)
  fix(terminal): resolve performance.now test leakage flake

## 0.8.3

### Patch Changes

- b315b6d: Make `claude --ide` actually work, and stop the IDE lock leaking the bridge token.
  
  #155 wired `write_ide_lock_file` into `iyke::start`, but the lock was malformed,
  world-readable, and pointed at a server that did not exist. Verified end to end
  against a real `claude --ide` session this time, not by unit test.
  
  - **The lock is now the shape Claude Code reads** — `pid`, `workspaceFolders`,
    `ideName`, `transport: "ws"`, `runningInWindows`, `authToken`, with the port
    as the file name. It previously wrote `{port, authToken, pid, lock_path}`,
    which the CLI cannot use.
  - **Written 0600.** It carries the iyke bridge bearer token and went out at 0644
    via a plain `fs::write`. `control.json` and the per-terminal hook settings
    both use an explicit private write for exactly this reason.
  - **A real MCP-over-WebSocket server** (`iyke::ide_ws`) now answers on the port
    the lock advertises, authenticating on `x-claude-code-ide-authorization`.
    Implements `initialize`, `tools/list` and `tools/call` for the tools the shell
    can honestly answer — `getWorkspaceFolders`, `openFile`, the selection pair
    and `getDiagnostics` — and returns an explicit error for the rest rather than
    a plausible-looking empty success.
  - **The `mcp` subprotocol is selected.** Claude Code sends
    `Sec-WebSocket-Protocol: mcp` and hangs up ~30ms after the handshake without
    sending a frame if the server does not select it, so nothing above the
    handshake can detect the failure.
  - **Stale locks are reaped at start.** `Drop` does not run on SIGKILL, and a
    stale lock points `claude` at a dead port. Reaping is narrow: only locks whose
    `ideName` is ours and whose pid is gone. `kill(pid, 0) == 0` alone was not
    enough — a live process we do not own fails with `EPERM`, which would have
    read as dead.
- 5d3f558: Make the terminal's live view of a Claude session actually receive events —
  cost HUD, tool-call feed, permission inbox, git ledger (#149).
  
  All four are mounted in `shell/panes/views/terminal-view.tsx` and none had ever
  received a single event, for three independent reasons:
  
  1. **Nothing was posting.** The hooks and `statusLine` were installed by the
     `CLAUDE_CONFIG_DIR` overlay, which hardcoded `port: 0` — so every hook fired
     `curl … http://127.0.0.1:0/iyke/hooks/event`. The overlay is gone; the wiring
     now rides `claude --settings <file>`, documented by the CLI as loading
     *additional* settings, layered over user/project/local rather than replacing
     them. The user's real `~/.claude` is discovered natively and never written.
  2. **Nothing could read.** `tool-call-feed.tsx`, `cost-hud.tsx` and
     `permission-inbox.tsx` each hardcoded `http://127.0.0.1:4000`, a port the
     bridge has never bound — it takes a dynamic one. They now go through
     `iykeFetch`, which resolves the live endpoint and bearer token.
  3. **One route did not exist.** `permission-inbox.tsx` POSTed to
     `/iyke/hooks/decision`, which was never registered, so the operator's
     approve/deny 404'd. Added as record-and-broadcast.
  
  The settings document is written by `iyke::hook_settings` at bridge start —
  the one moment the real port and token both exist — beside `control.json`, 0600
  because it carries a bearer token, and removed alongside it on clean shutdown
  (a `SIGTERM` skips unwinding and leaves both behind — verified, and identical to
  `control.json`'s existing behaviour; a stale file only ever points at a dead port,
  and the wrapper reads the path from the live endpoint, never from disk). A stale port
  is therefore impossible by construction rather than merely unlikely, and every
  curl in the document is authenticated. Only terminals Ikenga launches are wired;
  a `claude` started by hand in a plain shell is untouched.
  
  Two limits stated plainly. `/iyke/hooks/decision` is **record-only**: the
  `PreToolUse` hook has already returned `{"continue": true}` by the time a human
  sees the request, so it cannot retroactively gate the call — a real gate means
  holding that response open, which is a separate design. And the IDE lock file
  (`~/.claude/ide/<port>.lock`) is still unwired; `write_ide_lock_file` only ever
  got `port: 0` and the literal token `"ikenga-token"`, so IDE discovery has never
  worked either. Both tracked in #149.
- b4b45b6: Close the follow-ups left open by the terminal-continuity work: a permission
  gate that can actually gate, working `claude --ide` discovery, visible parked
  packages, and an opt-in terminal resume.
  
  - **A real `PreToolUse` gate (#154).** `/iyke/hooks/event` now holds the hook
    response open while a human decides in the permission inbox, opt-in per
    terminal via `permissions.hold_terminal_<id>` (default off). The three
    timeouts nest explicitly — server hold 30s < `curl --max-time` 35s < Claude
    Code hook timeout 40s — because a hold that outlives the hook's own curl is
    a gate that silently passes every tool call through. Deny is expressed as
    `hookSpecificOutput.permissionDecision`, not `continue: false`, so denying
    one tool call does not end the conversation.
  - **`claude --ide` discovery (#155).** `write_ide_lock_file` is wired into
    `iyke::start` with the live bridge port and token and removed on shutdown, so
    a stale lock cannot point `claude` at a dead port. It was previously correct
    but unreachable, and its only historical caller passed `port: 0` and a
    placeholder token.
  - **Parked packages are visible and recoverable (#157).** The activity-bar rail
    badges a parked pkg instead of showing an entry indistinguishable from a
    healthy one, and the pkg sidebar surfaces the park reason with a retry.
    `ActivityBarEntry` now carries the pkg name, so a multi-view pkg is
    identifiable by its own name rather than by its first `ui.nav` view.
  - **Opt-in terminal resume (#133).** A default-off `terminal.resume_on_start`
    setting; when enabled, rehydration respawns previously running tabs including
    those in unfocused panes, preserving the Claude session id, rather than
    waiting for someone to look at the pane.
- 5d3f558: Make Ikenga-launched Claude terminals continuous across refreshes and restarts,
  and fix hook attribution so each terminal tab sees only its own events.
  
  - **Per-terminal hook settings.** The `claude --settings` file is now written at
    PTY-spawn time (`claude-hooks-<terminalId>.json`) rather than once at bridge
    start, and every hook/statusline URL carries `?terminal=<id>`. Rust adds
    `ikenga_terminal_id` to each event, so the tool-call feed, permission inbox,
    git ledger, and cost HUD can distinguish terminal A from terminal B even when
    both share the same cwd.
  - **Refresh reattach.** `rehydrateFromDb` now calls `ptyTerminalList()` before
    respawning and restores the `ptyId` of any matching live PTY. `SingleTerminal`
    attaches via the existing atomic `Pty.attach` handshake instead of spawning a
    duplicate.
  - **Restart resume.** `SessionStart` hook `session_id` is captured and persisted
    on the terminal tab. When the tab is later respawned (app restart or manual
    restart), `claude` is launched with `--resume <session_id>` so the previous
    conversation returns.
  - **Statusline per terminal.** The `statusline://snapshot` event and
    `GET /iyke/statusline/snapshot` endpoint now carry a per-terminal map;
    `CostHud` filters to the terminal it belongs to.
  - **Daemon-backed terminals (#102): deferred.** This PR fixes continuity without
    moving PTYs into the server. Daemon-backed PTYs remain the right long-term
    architecture for multi-window / remote sessions and will be scoped as a
    separate work package against #102.
  - **Package npm dependency materialization (#150).** Registry, local, dev, and
    iyke installs now run `npm install --omit=dev` for packages that declare a
    `lifecycle: "long-lived"` MCP server, preventing `ERR_MODULE_NOT_FOUND` on
    first boot. Falls back to `bun install --production` if npm fails.

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
