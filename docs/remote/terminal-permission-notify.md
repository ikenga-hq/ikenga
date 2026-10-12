# Terminal Permission Notifications (T0/T1 Daemon)

**Gate:** WP-D (#452 Follow-up) · **Status:** DESIGN NOTE & ARCHITECTURAL PROPOSAL · **Date:** 2026-10-12

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

We evaluate four potential approaches:
1. **Approach A:** PTY Output Scraping (terminal buffer inspection)
2. **Approach B:** Config Overlays (`CLAUDE_CONFIG_DIR`, `--settings`, `~/.claude/settings.json`)
3. **Approach C:** In-Terminal Prompt Tracking (`TerminalPrompts`, #452 Desktop Port)
4. **Approach D:** Native Claude Code Plugin Hooks (via WP-09 Consolidated Plugin)

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

### 2.2 Approach B: Configuration Overlays (`CLAUDE_CONFIG_DIR`, `--settings`, `~/.claude/settings.json`)

#### Mechanism
Overriding configuration directories or command-line flags to inject hook definitions into Claude Code's startup configuration.

#### Rejection of `CLAUDE_CONFIG_DIR` Overlay (G-05, v0.8.2)
- **Login Session Breakage:** In Claude Code v0.8.2, setting a `CLAUDE_CONFIG_DIR` overlay caused the runtime to lose the user's primary login tokens and OAuth session state, forcing interactive re-login prompts.
- **Architectural Prohibition:** Principle G-05 explicitly prohibits `CLAUDE_CONFIG_DIR` overlays for this exact reason.
- **User Config Drift:** Global overrides in `~/.claude/settings.json` or mutable `--settings` arguments conflict with user customizations, get overwritten during agent git cleanups, and leave dangling hook definitions after Ikenga daemon shutdown.

#### Reconciliation with Daemon Hook Settings
The daemon avoids global config overlays by generating a dedicated, per-terminal settings file (`hook_settings`) stored in a private runtime directory, scoped specifically to the individual session without altering the user's persistent `~/.claude/` state or overriding `CLAUDE_CONFIG_DIR`.

**Assessment:** Strictly rejected for configuration overlays; private per-terminal hook settings used where necessary.

---

### 2.3 Approach C: Existing Desktop `TerminalPrompts` Path (#452 Follow-up)

#### Mechanism
The Ikenga desktop already implements structured in-terminal prompt detection and tracking: `TerminalPrompts` (`src-tauri/src/iyke/hooks.rs:310`).
- It tracks active prompt state directly within the terminal subsystem.
- When Claude Code prompts for input, `TerminalPrompts` records the prompt as an active row in the permission system.
- Issue #452 explicitly identified porting `TerminalPrompts` to the headless daemon as an operational path.

#### Evaluation
- **Strengths:**
  - Zero external plugin dependencies or filesystem overlay requirements.
  - Tightly coupled with the terminal session lifecycle; prompt state naturally expires when the terminal closes.
  - Battle-tested in the desktop application.
- **Headless Requirements:**
  - On a headless daemon, `TerminalPrompts` must interface directly with `server::hook_asks` and `server::shared::notifications::routing` rather than desktop Tauri events.
  - Must deliver prompt notifications out-of-band to Web Push and companion devices, and ensure that unattended prompts cleanly time out and record audit entries.

**Assessment:** A primary, viable native path for daemon-side terminal prompt tracking.

---

### 2.4 Approach D: Native Claude Code Plugin Hooks (via WP-09 Consolidated Plugin)

#### Mechanism and Consolidation Under WP-09 (G-07)
Claude Code supports hook declarations via plugin manifests. Rather than creating a disjoint standalone `ikenga-claude-hook` package, principle **G-07 establishes that all Ikenga agent tools and extensions are consolidated into a single Ikenga plugin** (WP-09). The terminal hook functionality must ride inside WP-09's single plugin.

#### Correct Hook Mechanics: `hooks.json` and `PreToolUse`
1. **Manifest Declaration:**
   - Plugins declare hooks in `hooks.json` (not through custom Node wrapper scripts or `package.json` entry points).
   - The manifest declares listeners for events such as `PreToolUse`.
2. **Permission Decision Protocol:**
   - In `PreToolUse`, tool gating is handled via the `permissionDecision` field in `hookSpecificOutput`:
     ```json
     {
       "continue": true,
       "hookSpecificOutput": {
         "hookEventName": "PreToolUse",
         "permissionDecision": "allow" | "deny"
       }
     }
     ```
   - **Critical Invariant:** The hook response must set `"continue": true`. Setting `"continue": false` aborts the entire Claude conversation / session. To deny an individual tool call (such as a Bash command), the hook returns `"continue": true` with `"permissionDecision": "deny"` and an explanatory message Claude can respond to.

---

## 3. Threat Model and Anti-Spoofing Analysis

### 3.1 PTY Environment Token Exposure
Any environment variable placed into the PTY environment (such as `IKENGA_HOOK_TOKEN` or a Unix socket path) is **inherited by every process spawned within that terminal**:
- Commands run by the agent (e.g. `bash`, `python`, `npm test`, arbitrary build scripts) can inspect their own environment (`/proc/self/environ` or `env`).
- A rogue script or untrusted build process running inside the terminal PTY can read `IKENGA_HOOK_TOKEN` and the socket path, and construct an authenticated, syntactically valid `PermissionRequest` to the daemon's IPC socket.
- **Conclusion:** Environment variable bearer tokens in the PTY environment do **not** form an impenetrable boundary against malicious code executing within that same PTY.

### 3.2 Spoofing Mitigations
To maintain defense-in-depth:
1. **Operation Binding:** The daemon must correlate incoming hook requests with active tool execution state in the terminal supervisor.
2. **Short-Lived Ephemeral Nonces:** Use single-use interaction nonces tied to specific, un-gated operations rather than long-lived static tokens.
3. **Transparent Audit Trails:** Prompts and decisions are logged with full command arguments and working directories, ensuring any prompt generated inside the PTY is visible in the immutable audit ledger.
4. **Owner Verification:** In multi-tenant systems, prompts cannot elevate privileges beyond the session owner's capability set.

---

## 4. T1 Multi-Principal Isolation and Shared-Project Routing

In T1 multi-user mode, multiple principals share a host, and projects may be shared across team members.

### 4.1 UID and Socket Isolation
- Each principal child runs under its own Linux UID (`ik-<user>`).
- Hook sockets are created in private per-principal runtime directories (`/run/user/<uid>/ikenga/`) with permissions `0700` / `0600`.
- Linux DAC prevents cross-principal access to sockets and IPC channels.

### 4.2 Shared-Project Owner Routing
When an agent executes within a project shared between members (e.g. member A works within owner O's project):
- The ask record includes `requested_by` (member A) and `project_id` (owner O's project).
- Per G-ACCESS §5.7, sensitive tool operations (sensitivity >= 1) require project owner authorization and cannot be approved by a tenant member alone.
- The daemon's routing subsystem (`server::shared::notifications::routing::route_ask` and `decide_with`) directs the prompt to the authorized decider (the project owner or authorized delegates) and records the deciding principal in `audit_events`.
- Prompts never leak to unrelated projects or tenant principals.

---

## 5. Summary Recommendation

1. **Reject Config Overlays:** Strictly uphold G-05; do not use `CLAUDE_CONFIG_DIR` or global `~/.claude/settings.json` overlays.
2. **Consolidate with WP-09:** Ride on WP-09's single consolidated Ikenga plugin (G-07) using standard `hooks.json` and `PreToolUse` `permissionDecision` semantics (`continue: true`).
3. **Bridge with `TerminalPrompts`:** Combine native in-terminal prompt tracking (`TerminalPrompts`, #452) with daemon out-of-band notification routing (`server::hook_asks`).
4. **Acknowledge PTY Environment Visibility:** Treat in-terminal tokens as process-level credentials, mitigated by active command correlation, DAC isolation, and project-owner routing in T1 shared projects.
