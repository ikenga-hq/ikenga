# Terminal Permission Notifications (T0/T1 Daemon)

**Gate:** WP-D (#452 Follow-up) · **Status:** DESIGN NOTE & ARCHITECTURAL PROPOSAL · **Date:** 2026-10-11

---

## 1. Context and Problem Statement

When an AI coding assistant (principally Claude Code) runs inside an interactive terminal (PTY) on a headless remote Ikenga box (T0 single-operator or T1 multi-user), it frequently reaches tool calls that require operator consent (e.g. executing bash commands, writing outside the workspace, or accessing sensitive credentials).

In a local desktop session, the operator is either looking directly at the terminal window or receives in-app toasts/prompts via desktop IPC. On a remote headless server, however:
- The operator may have disconnected the terminal view, switched tabs in the web client, or be monitoring the server via the mobile Companion application or Web Push.
- If a permission prompt blocks inside the terminal PTY, the session hangs waiting for interactive keyboard input (`[y/n]`).
- Without out-of-band notification, the ask neither reaches the Ikenga notification center (`shell_notifications`), nor the push notification gateway, nor the immutable access audit chain (`audit_events` / `access.db`).

The goal of this design note is to evaluate mechanisms for reliably intercepting terminal-based permission prompts, routing them through Ikenga's notification and approval core (`server::shared::notifications::routing` and `server::hook_asks`), enabling remote approval/denial, and recording full attribution in the audit chain.

---

## 2. Evaluation of Architectural Approaches

We evaluate three potential approaches:
1. **Approach A:** PTY Output Scraping (terminal buffer inspection)
2. **Approach B:** Claude Code Plugin via `CLAUDE_CODE_PLUGIN_DIRS`
3. **Approach C:** Process Wrapper / Shim / Ptrace Interception

---

### 2.1 Approach A: PTY Output Scraping

#### Mechanism
The daemon PTY supervisor (`pty::manager` or `PtySession`) reads the raw master PTY stream. It matches byte patterns or ANSI-stripped lines against regex patterns characteristic of Claude Code prompts (e.g. `Allow [A-Za-z0-9_-]+ to run \?`, `\(y/n\)`, or ANSI cursor-movement sequences for interactive option selection). Upon matching:
1. A synthetic permission notification is recorded.
2. When the operator responds via web/companion, the daemon writes the corresponding key sequence (`y\n` or `n\n`) into the PTY slave via `write_stdin`.

#### Vulnerabilities and Failure Modes
1. **Formatting Fragility & Terminal Emulation:**
   - Claude Code utilizes sophisticated terminal rendering libraries (Ink / React-in-terminal, curses-like alternate screens, color themes).
   - Terminal resizing (`SIGWINCH`), line wrapping, cursor jumps (`\x1b[2K`, `\x1b[1A`), and progress spinners corrupt raw byte sequences.
   - Upstream text modifications or internationalization instantly break regex matching.
2. **Prompt Spoofing (Severe Security Hazard):**
   - Any process running inside the terminal (a script executed by the agent, or untrusted output printed by `cat untrusted_file.txt`) can emit ANSI sequences and strings identical to Claude Code's permission prompt.
   - An attacker or rogue script could trick the PTY scraper into presenting a fake prompt to the operator (or vice-versa, causing Ikenga to inject auto-approval keystrokes into an arbitrary shell command).
3. **Bi-directional Stdin Clashing:**
   - If the operator is concurrently typing in the terminal while the remote notification is resolved, stdin bytes interleave, producing malformed input or unintended command execution.

**Assessment:** Disqualified for security-sensitive permission routing.

---

### 2.2 Approach B: Claude Code Plugin via `CLAUDE_CODE_PLUGIN_DIRS`

#### Mechanism
Claude Code includes a native plugin and hook mechanism. Rather than modifying mutable user configuration files (such as `~/.claude/settings.json` or `--settings` flags, which can be overwritten by user edits or agent actions), Ikenga's process executor injects the environment variable `CLAUDE_CODE_PLUGIN_DIRS` when spawning the agent PTY.

This environment variable points to a daemon-managed, read-only directory containing an Ikenga permission bridge plugin:
- The plugin registers Claude Code extension hooks: `PreToolUse`, `PostToolUse`, `Notification`, and `PermissionRequest`.
- On `PreToolUse` or `PermissionRequest`, the plugin pauses tool execution and transmits structured JSON metadata over an authenticated local IPC transport (Unix domain socket or loopback HTTP) to `ikenga-server`.
- The daemon holds the request in `server::hook_asks`, generates a `shell_notifications` row, emits events to connected clients and push notification channels, and waits for a decision.
- Once decided (`allow_once`, `allow_always_project`, `deny`), the daemon responds over the IPC connection, and the plugin returns `{ continue: true }` or `{ continue: false, error: ... }` to Claude Code.

#### Why `CLAUDE_CODE_PLUGIN_DIRS` over Config Overlays
1. **Immutability & Zero Drift:**
   - Config overlays written to user home directories (`~/.claude/settings.json`) suffer from drift, user collisions, and deletion during agent git cleans.
   - Environment variables injected at process launch are atomic, read-only to the child shell unless explicitly unset, and do not persist stale state on disk across runs.
2. **Deterministic Versioning:**
   - The plugin code is packaged with the Ikenga release binary (`/usr/lib/ikenga/plugins/claude-bridge` or alongside the daemon asset root). Upgrading Ikenga upgrades the bridge atomically.

#### T1 Multi-Account Isolation
In T1 mode, multiple system users (`ik-user-1`, `ik-user-2`) run concurrently on the same host, each mapped to a distinct Unix UID and principal ID:
1. **Per-Principal Socket / Port Isolation:**
   - Each terminal session is provided with a dedicated Unix Domain Socket in its private runtime directory (e.g. `/run/user/<uid>/ikenga/hook-<session_id>.sock` with mode `0600` owned by that principal's UID).
   - Alternatively, communications route to the principal's own child daemon (`ikenga-server` running under that user's UID), which listens on a private loopback socket or inherits the connection.
   - Principals cannot connect to or read another user's hook sockets due to standard Linux DAC permissions.
2. **Unguessable Per-Session Hook Bearer Token:**
   - The daemon mints a high-entropy secret token (`IKENGA_HOOK_TOKEN`) per terminal spawn.
   - The plugin includes this token in all IPC headers (`Authorization: Bearer <token>`).
   - The daemon rejects any hook payload whose token does not match the active session.

#### Anti-Spoofing Guarantees
- Terminal output (stdout/stderr) is completely decoupled from the permission control plane.
- An untrusted bash command outputting fake permission text has zero capability to trigger an Ikenga notification, because notifications are strictly generated from structured JSON events received over the authenticated IPC channel.

**Assessment:** Highly recommended. Robust, secure, and aligns with standard agent architecture.

---

### 2.3 Approach C: Shell / Binary Wrapper (Shim / LD_PRELOAD)

#### Mechanism
Installing a wrapper script in `PATH` (e.g. `/usr/local/bin/claude`) or injecting an `LD_PRELOAD` shared object that hooks libc `read`/`write`/`connect` calls.

#### Drawbacks
- Bypassed if the user or agent executes Claude via alternative binaries or package managers (`npx @anthropic-ai/claude-code`, `/home/user/.nvm/.../bin/claude`, or direct `node` invocation).
- High maintenance overhead and fragility across Node.js runtime updates.

**Assessment:** Inferior to native plugin integration.

---

## 3. Comparison Matrix

| Criterion | (a) PTY Output Scraping | (b) `CLAUDE_CODE_PLUGIN_DIRS` Plugin | (c) Wrapper / Shim |
| :--- | :--- | :--- | :--- |
| **Reliability** | Very low (fragile ANSI, resize, spinners) | High (structured JSON events) | Medium (bypassed by path) |
| **Spoofing Resistance** | **None** (untrusted stdout can spoof prompt) | **High** (authenticated IPC bearer token) | Medium |
| **T1 Multi-Account Isolation** | Low (relies on PTY session mapping) | **High** (UID-isolated sockets + session tokens) | Medium |
| **Configuration Cleanliness** | Zero config required | **Clean** (injected env var, no disk pollution) | Intrusive (PATH or LD_PRELOAD) |
| **Audit Fidelity** | Poor (text heuristic only) | **Full** (exact tool name, arguments, cwd) | Poor |
| **Upstream Compatibility** | Breaks on any UI/CSS update | Stable across Claude Code versions | Fragile to Node/runtime changes |

---

## 4. Concrete Recommendation and Architecture

### 4.1 Recommendation
We recommend implementing **Approach B: Claude Code Plugin via `CLAUDE_CODE_PLUGIN_DIRS`**.

### 4.2 Proposed Implementation Architecture

1. **Plugin Distribution:**
   - Package a minimal, zero-dependency Node.js plugin inside `ikenga-server` distribution assets at `<install_dir>/plugins/ikenga-claude-hook/`:
     - `package.json`
     - `index.js`
   - The plugin hooks into Claude Code's extension points:
     - `PreToolUse`: For shell commands and sensitive tool calls.
     - `PermissionRequest`: When Claude explicitly yields for human permission.

2. **Terminal Spawning Configuration (`src-tauri/src/pty/mod.rs` & `src-tauri/src/server/`):**
   - When spawning an interactive terminal or Claude Code session, the executor injects:
     ```bash
     CLAUDE_CODE_PLUGIN_DIRS="<install_dir>/plugins/ikenga-claude-hook"
     IKENGA_HOOK_IPC_PATH="/run/user/<uid>/ikenga/hooks-<terminal_id>.sock"
     IKENGA_HOOK_TOKEN="<minted_uuidv7_token>"
     IKENGA_TERMINAL_ID="<terminal_id>"
     ```

3. **IPC Transport & Daemon Handler:**
   - In T0: The daemon listens on a local Unix domain socket or verifies the bearer token on `/iyke/hooks/event`.
   - In T1: The principal child process owns the socket at `/run/user/<uid>/ikenga/hooks-<terminal_id>.sock` (mode `0600`).
   - The payload format matches `HookPayload` handled by `src-tauri/src/server/hook_asks.rs` and `src-tauri/src/iyke/hooks.rs`.

4. **Integration with Existing Subsystems:**
   - **Notification Center:** Every held ask produces a row in `shell_notifications` with `kind = 'permission'` and `dedupe_key = 'permission:hook:<request_id>'`.
   - **Remote Decisions:** Web and Companion clients call `permission_decide` or `term_hooks_decide`.
   - **Audit Chain:** Successful decisions append `permission.decided` to `audit_events`. Timeouts, crashes, or refusals append `permission.refused` with their exact outcome (`timed_out`, `expired:restart`, `hook_disconnected`).
