# Terminal Permission Notifications (T0/T1 Daemon)

**Gate:** WP-D (#452 follow-up) · **Status:** DESIGN NOTE · **Date:** 2026-10-13

Paths below are relative to `shell/src-tauri/src/` unless they start with `plans/` or `docs/`. Every
`file:line` was checked against the tree this note ships in. Where a claim comes from outside this repo
(Claude Code's own hook and plugin formats) it says so.

---

## 1. Context and problem

An agent such as Claude Code, running in a terminal (PTY) on a headless Ikenga box (T0 single operator or
T1 multi-user), reaches tool calls that need a human's consent. On a desktop the operator is looking at
the window. On a remote box they may have the terminal tab closed and be on the phone or the Companion app.

A permission prompt that nobody sees blocks the session, and it never reaches the notification centre
(`shell_notifications`), Web Push or the audit chain (`audit_events`).

This note records which mechanisms we considered for getting terminal permission asks onto those surfaces
(`server/shared/notifications/routing.rs` and `server/hook_asks.rs`), which one we use, and what WP-D's audit
fixes guarantee.

---

## 2. Approaches considered

1. **A.** Scrape the PTY output.
2. **B.** Change Claude Code's configuration (`CLAUDE_CONFIG_DIR`, `--settings`, user `settings.json`).
3. **C.** Track Claude Code's own in-terminal prompts (`TerminalPrompts`, desktop only today).
4. **D.** Ship the hooks in an Ikenga Claude Code plugin (not built).

### 2.1 A. PTY output scraping: rejected

The daemon would read the PTY master stream (`pty/mod.rs`, `PtyManager` at `:369`, sessions at `:370`),
match something that looks like a prompt, record an ask, and answer it by writing `y` or `n` back into the
PTY (`PtyManager::write`, `pty/mod.rs:1165`).

- **Fragile.** Claude Code draws its UI with a terminal framework: alternate screens, cursor jumps,
  spinners, wrapping on resize. Any wording or layout change breaks the match.
- **Spoofable.** Any process in the terminal can print text that looks identical to a prompt. Ikenga would
  then show the operator a fake ask, or type approval keystrokes into whatever is reading stdin.
- **Races the human.** If the operator types in the terminal while the remote answer arrives, the bytes
  interleave.

### 2.2 B. Configuration changes: `CLAUDE_CONFIG_DIR` rejected, per-terminal `--settings` used

Two different things get called "config injection", and we treat them differently.

**Rejected: a `CLAUDE_CONFIG_DIR` overlay (gap G-05).**

- This was shipped in the *Ikenga desktop shell*, and removed in Ikenga **v0.8.2** (2026-08-28, issue #149 /
  PR #151; `STATUS.md` in the workspace root, "v0.8.2 — the terminal `CLAUDE_CONFIG_DIR` overlay"). The
  version number is Ikenga's release, not Claude Code's.
- Cause: the overlay directory symlinked the user's credentials and project map in under the wrong file
  names, so `claude` in an Ikenga terminal asked to log in and trust the folder again, and wrote a second
  config. Its hooks also pointed at `http://127.0.0.1:0` and so failed on every tool call.
- The uplift plan locks the rule: plugins load "only through `claude plugin install` or
  `CLAUDE_CODE_PLUGIN_DIRS`. Never through a `CLAUDE_CONFIG_DIR` overlay"
  (`plans/2026-10-04-claude-platform-uplift/01-plan.md:246`, gap G-05 at `:286`). The PTY code carries the
  same warning (`pty/mod.rs:567`).
- We also do not write the user's `~/.claude/settings.json`.

**Used: one `--settings <file>` per terminal.**

- `claude` is started with `--settings <file>`; that flag adds hooks and a statusline for that one process
  and leaves the config directory, login and trust alone (`server/term_hooks.rs:5-11`). The daemon writes
  the file itself for each terminal (`TermHooks::wire`, `server/term_hooks.rs:300-346`), into a 0700
  directory under `--data-dir` (`make_private_dir`, `:549-565`), as a 0600 file (`write_private`, `:569`).
- So the earlier wording "reject `--settings`" was wrong. What we reject is replacing or seeding the
  config directory. The per-terminal `--settings` file is the mechanism the daemon uses today.
- The file lists the hook events in `HOOK_EVENTS` (`server/shared/hook_settings.rs:44`), each a `curl`
  to the daemon over loopback TCP (see §4.1).

### 2.3 C. `TerminalPrompts` (desktop): useful, but not independent of B

`TerminalPrompts` (`notifications/producers.rs:171`) is pure bookkeeping. It does not detect prompts.
It is fed by Claude Code's `PermissionRequest` hook: `iyke::hooks::post_hook_event` receives that hook and
calls `terminal_prompts().lock()` then `.requested(...)` (`iyke/hooks.rs:99`, `:314`). The hook reaches the
desktop through the same per-terminal `--settings` file as §2.2. So this approach needs the hook file and
is not free of it.

- Today it is desktop only. The daemon publishes a `PermissionRequest` to the event bus and nothing more
  (`server/term_hooks.rs:19`, `:676`); it does not write a row for it (`server/hook_asks.rs:41-43`).
- Porting it to the daemon means an in-terminal prompt gets a `permission` row, but the answer still
  happens in the terminal: Claude Code's own prompt has no remote answer path. Only a held `PreToolUse`
  can be answered remotely (§2.5).

### 2.4 D. Hooks inside an Ikenga Claude Code plugin (WP-09): future, not built

- WP-09 is planned (`plans/2026-10-04-claude-platform-uplift/01-plan.md:278`). Its scope is the
  mcp-iyke `.mcpb` plus a mod: status line, `/ikenga` commands, toasts and `prompt.compose` context
  (`01-plan.md:144-149`, `05-tracking.md:272-282`). Permission hooks are not in that scope.
- Its load path is fixed: `claude plugin install`, or the `CLAUDE_CODE_PLUGIN_DIRS` environment variable that
  WP-11 sets on the PTY (`01-plan.md:148`, `:95`). The PTY is started with an empty environment plus
  the caller's, so the variable must be set explicitly (`pty/mod.rs:553-561`).
- If permission hooks later move into a plugin, they belong in that single plugin. Format, from Claude Code's
  documentation rather than this repo: the plugin manifest is `.claude-plugin/plugin.json`; hooks are declared
  in `hooks/hooks.json` (or inline in the manifest). `hooks.json` is not the manifest.
- Until then the per-terminal `--settings` file (§2.2) is the only mechanism, and nothing here depends on
  WP-09.

### 2.5 The hook protocol we rely on

A held `PreToolUse` answers with the decision in `hookSpecificOutput.permissionDecision`
(`iyke/hooks.rs:279-305` builds it for the desktop; the daemon answers with the desktop's exact response, `server/term_hooks.rs:61-62`):

```json
{
  "continue": true,
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow" | "deny"
  }
}
```

- **`"continue": true` is required.** `"continue": false` ends the whole Claude session, not just the tool call
  (comment at `iyke/hooks.rs:279-283`). A denial is `continue: true` with `permissionDecision: "deny"` and a
  reason Claude can read.
- Claude Code also accepts `"ask"` for `permissionDecision` (hands the decision back to Claude Code's own
  prompt; from its hook documentation). Ikenga emits only `allow` and `deny`.
- A hold is opt-in per terminal by the setting `permissions.hold_terminal_<id>` (`server/term_hooks.rs:55-56`,
  `:708-715`). It lasts at most `GATE_HOLD_SECS` and an unanswered ask is denied, never allowed.

---

## 3. Threat model: who can forge a hook

### 3.1 The per-terminal secret is readable from inside the terminal

Every hook is authenticated by a per-terminal secret: 256 random bits, minted in `TermHooks::wire`, sent as
`Authorization: Bearer <secret>`. It is carried in a 0600 header file that the hook's `curl -H @file` reads,
never in curl's argv (`server/term_hooks.rs:29-44`, `server/shared/hook_settings.rs:60-65`).

That file is owned by the same uid the PTY runs as (`server/term_hooks.rs:35-38`). So **any process in the
terminal can read it.** The path is in the settings file, and the settings file's path is in `claude`'s own
argv, which any process of that uid can read. A script that the agent runs, or a build step in the same
terminal, can therefore post a forged `PermissionRequest` or a forged event for that terminal today. The 0600
mode and the 0700 directory keep out *other* uids, not other processes of this one.

What the secret does protect:

- It opens that terminal's two routes only (`EVENT_PATH`, `STATUSLINE_PATH`), not the daemon's RPC.
  It is neither the daemon's `IKENGA_AUTH_TOKEN` nor a device token (`server/term_hooks.rs:29-32`).
- The terminal id comes from the `?terminal=` query and the secret must be that terminal's; wrong secret,
  unknown terminal and missing header are one indistinguishable 401 (`require_terminal_secret`, `:620-639`;
  `authenticates`, `:407`; `unauthorized`, `:602`).
- It is revoked when the PTY exits (`TermHooks::revoke`, `:356`).

The daemon's own credentials are not in the PTY environment. The PTY starts from an empty environment, minus
every key `is_host_only_env` names (`pty/mod.rs:553-561`; list at `:203-210`: `IKENGA_AUTH_TOKEN`,
`IKENGA_VAULT_KEY`, `IKENGA_PKG_DB_TOKEN`, the principal wrapping key, `IKENGA_SECRET_*`).

**Conclusion.** The hook secret is a process-level credential for one terminal. It does not separate the
agent from code the agent runs in that terminal. If we ever moved the secret into an environment variable
(for example `IKENGA_HOOK_TOKEN`), the exposure would be the same, plus `/proc/<pid>/environ` and inheritance
by every child.

### 3.2 What limits the damage

1. **A forged hook cannot approve anything.** An approval is `permission_decide` or `term_hooks_decide`,
   which need the `approve` cap on an authenticated credential (`server/term_hooks.rs:62-64`). A forged
   `PermissionRequest` can at worst make a bogus row appear in the inbox, and a forged event can make the
   HUD lie. Nobody can allow a tool call by posting to the hook endpoint.
2. **Decisions are single use.** The held request is removed from the map before it is answered
   (`server/term_hooks.rs:58-60`), so a second answer gets `gated: false`.
3. **Every outcome with no decider is audited** (§5), so a hold that timed out, a table that was full, a
   terminal that ended and a hook that hung up each leave a row.
4. **What the audit row holds is small.** The `target` is the title `Claude wants to use <tool>` (at most 120
   characters; `server/shared/notifications/hook_ask.rs:66`, `routing.rs:1162`). The `detail` is names and ids
   (decision, outcome, sensitivity level, requester) and is refused if over 2048 bytes
   (`access/audit/list.rs:46`, `:509`). Neither the command line nor the working directory is recorded, so
   the chain shows that an ask happened and how it ended, not what was in it.
5. **Owner verification in T1 shared projects** (§4.2).

There is no "operation binding" or "active command correlation" between a hook and the commands running in
the terminal, and no per-ask nonce. Those are ideas, not mechanisms, and nothing here relies on them.

---

## 4. T1: principals and shared projects

### 4.1 Isolation between principals

- Each principal has its own child daemon, running as that principal's own Linux user (`ik-<name>`;
  `UNIX_NAME_PREFIX`, `server/operator/accounts.rs:162`, and `unix_name_for`, `:205-212`). A child has its own router, its own
  `TermHooks`, and its own bus (`server/term_hooks.rs:46-51`).
- **The hook transport is loopback TCP, not a Unix socket.** The settings file's `curl` commands post to
  `http://127.0.0.1:<port>/term-hooks/event?terminal=<id>`
  (`server/shared/hook_settings.rs:69`, `:116-121`). Any uid on the host can open that port.
- So the isolation between principals is the secret, not file permissions on a socket. Another principal who
  finds the port gets the 401 of §3.1; a secret is known only to the child that minted it. The secret lives in
  a 0700 directory under the child's own `--data-dir`, so other uids cannot read the file
  (`server/term_hooks.rs:549-565`).

### 4.2 Shared projects: who may answer

A member can work in an owner's project. The ask is recorded with the project id and a sensitivity level
(`server/hook_asks.rs`, attribution via `server/shared/notifications/hook_ask.rs:115`). The rules are in
`can_decide` (`server/shared/notifications/routing.rs:831`), reached through `decide_with` (`:1003`):

- A member must be in the ask's project (`NotInShare`, `:847-849`).
- **Secret-material asks (level 2) always need the owner**, whatever the project's policy (`:852-856`).
- **Sensitive asks (level 1) need the owner only if the project's `owner_approval_required` flag is on**
  (`share.owner_approval`, `:858`). That is a per-project setting, stored in `shared_projects`, default on
  (`access/policy.rs:73-83`), not an always-on rule.
- A member cannot make an `allow_always_project` decision (`:862-864`).
- **`requested_by` is always `None` for a terminal ask.** The terminal's asks are the owner's own work
  (`server/shared/notifications/hook_ask.rs:115-125`), so the row does not name a member as requester. The
  member who *decides* is recorded on the audit row (`Decider`, `routing.rs`), not the requester.

There is no function called `route_ask`; the decide path is `decide_with` / `can_decide`.

---

## 5. What WP-D guarantees: terminal asks that end without a decider are audited

The audit chain is written by one process per tier. T0: the daemon, from its own access store. T1: the broker,
from notes a principal child queues (`access/audit/child.rs`). The desktop calls the daemon
(`access_audit_record_local`).

| Case | Where | What is recorded |
|---|---|---|
| A held ask times out, its table is full, its terminal ends or its hook hangs up (daemon) | `server/hook_asks.rs`, `Outcome` | `permission.decided {deny, outcome}` or `permission.refused {hook_disconnected}` |
| An ask was open when the process died (daemon, boot sweep) | `sweep_orphans_before` `:302`, `audit_swept_ask` `:321` | `permission.refused {expired:restart}` |
| A held ask times out (desktop) | `settle_gate` `iyke/hooks.rs:516`, `audit_hook_gate_timeout` `:543` | `permission.decided {deny, timed_out}` |
| A decision with no open row (desktop) | `handle_no_row_decision` `iyke/hooks.rs:465` | `permission.decided` or `permission.refused {routing_refused}` |
| An ask was open when the desktop died (boot sweep) | `sweep_orphans_with` `notifications/mod.rs:325` | `permission.refused {expired:restart}` |

### 5.1 The expiry audit cannot be lost, and is written at least once

A swept ask is left `decided_via = 'swept-unaudited'` (`routing.rs:1660`, doc above it). A later boot sweeps
it again. It is closed (`'swept'`, `mark_swept_audited`, `routing.rs:1634`) only after its audit row is on the
chain:

- **T0:** `audit_swept_ask` writes with `try_append_audit` (`routing.rs:1186`, which returns the error that
  `append_audit` only logs) and closes the row only if that succeeded (`server/hook_asks.rs:321`). A failed
  write leaves the row open for the next boot.
- **T1:** the child cannot write the chain. Its note carries the row's id (`Note::ack_id`,
  `access/audit/child.rs:108`). The broker's `handle_batch` (`:387`) appends each note and sends back the ids
  of those it **appended**, as `POST /internal/audit/events/ack {"ids":[…]}`. The child's `ack_handler`
  (`:227`) closes exactly those rows. A note the broker refused or failed to write is not in the list; a note
  the child dropped from its 256-entry queue was never sent, so its row is still open. In both cases the
  next boot audits it again.
- **Desktop:** `sweep_orphans_with` calls `access_audit_record_local` with retries and closes the row only on
  success. If no access store exists on disk (the ephemeral fallback), no audit can land, so the row is
  closed without trying, instead of spending about a minute per ask at every boot
  (`notifications/mod.rs:298-310`). The same applies to a daemon with no audit sink (`AuditSink::Off`).

The guarantee is **at least once**. A crash between the append and the close writes the same
`expired:restart` row twice on the next boot. It is not exactly once.

### 5.2 Known limits

- `decided_via` holds `swept-unaudited` and `swept` for these rows. Migration 0070 documents the column as
  session, device or operator. The values are internal and are never shown.
- A closed channel on a desktop gate (as opposed to a timeout) is audited with `outcome: timed_out`.
- The `ExpiredRestart` arm in `hook_asks.rs` `audit_outcome` is unreachable; the sweep builds its note
  directly.
- The desktop's `iyke::hooks` and `notifications` modules compile only with the `desktop` feature, so the
  headless test build does not run their tests. Their logic is split into functions that take the daemon
  channel and the host as arguments (`sweep_orphans_with`, `settle_gate`, `handle_no_row_decision`) so the
  desktop test build can run it without an `AppHandle`.

---

## 6. Summary

1. **Do not replace or seed Claude's config directory.** No `CLAUDE_CONFIG_DIR` overlay, no writes to the
   user's `settings.json` (G-05, v0.8.2). The per-terminal `--settings` file is how hooks get in.
2. **Treat the hook secret as a process-level credential for one terminal.** It keeps other uids and other
   terminals out. It does not keep out code running in the same terminal, today or with an environment
   variable.
3. **Nothing is approved by a hook.** Approval needs an authenticated `approve` decision; the hook endpoint can
   only add rows and events.
4. **Every ask that ends with no decider is audited, and the audit is retried until it lands**
   (at least once).
5. **WP-09 is the place for any future plugin-delivered hooks**, loaded through `claude plugin install` or
   `CLAUDE_CODE_PLUGIN_DIRS`. It is not built, and nothing here depends on it.
