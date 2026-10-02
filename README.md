# Ikenga

[![Build](https://img.shields.io/badge/build-passing-brightgreen.svg)](https://github.com/ikenga-hq/ikenga/actions)
[![Version](https://img.shields.io/github/v/release/ikenga-hq/ikenga?label=version&color=blue)](https://github.com/ikenga-hq/ikenga/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Discussions](https://img.shields.io/badge/community-discussions-5865F2.svg)](https://github.com/ikenga-hq/ikenga/discussions)
[![Newsletter](https://img.shields.io/badge/newsletter-Building%20in%20the%20Loop-e8590c.svg)](https://buildingintheloop.substack.com/subscribe)

> An open-source desktop workspace for the multi-agent way of working — your agents,
> skills, commands, scheduled jobs, and memory in one window, all aware of each other.

<!-- SCREENSHOT: docs/media/hero.png — full app window: Home canvas with a couple of widgets -->
![Ikenga desktop](docs/media/hero.png)

## Why Ikenga?

A single chat box can't run a standing workforce. The moment your work outgrows one
conversation — recurring jobs, a handful of specialized agents, slash commands you reach
for every day, memory that should survive the session — a chat window starts losing the
thread. You end up with a drawer of tools that don't know about each other.

Ikenga is the workspace those parts live in. One window: a terminal, a chat, and a file
viewer side by side; agents, skills, and commands as named files you can read; scheduled
jobs and a persistent memory layer running underneath. We've run a real operation on this
setup for over a year — about 95 skills, dozens of agents, scheduled jobs, and persistent
memory — and pulled the whole thing into one place. Now it's open.

## What it is

`ikenga` is a Tauri 2 + Vite + React 19 + TypeScript desktop app — a single-window control
plane that hosts terminals, AI chat sessions, viewers, and mini-apps. It's **engine-optional**:
the default AI engine (Claude Code) ships as a package, so it updates independently of the
shell, and the shell runs without any engine at all. Other engines — Codex, OpenRouter,
Antigravity, OpenCode, and Pi — are optional engine pkgs; see [Multi-engine chat](#multi-engine-chat)
for what each one needs and how mature it is.

Everything beyond the window chrome — mini-apps, tool servers, engine adapters — is an
independently installable **package (pkg)**. At boot the kernel discovers installed pkgs,
validates each manifest, and registers what it contributes: UI routes, MCP tool servers,
sidecars, cron, skills. The shell stays generic; it carries no vertical assumptions.

```
Pkgs (independently installable + updatable)
  ├─ UI pkgs       — iframe / webview mini-apps
  ├─ MCP pkgs      — headless tool servers
  ├─ CLI pkgs      — bundled sidecar binaries
  └─ Engine pkgs   — Claude Code adapter (default) plus alternative engines

Pkg Kernel (Rust + TS)  — manifest, lifecycle, scopes, IPC, updater
Shell Core              — chrome, identity, pkg manager UI
Tauri host              — SQLite, FS, native menu, secrets (Stronghold)
```

It's **local-first** and open source under **Apache-2.0** — both the platform and the
first-party pkgs.

## Install

```bash
curl -fsSL https://ikenga.dev/install.sh | sh
```

The script installs on Linux (x86_64) and Windows (under Git Bash). On macOS, download
the universal `.dmg` (Apple Silicon and Intel) from the
[Releases page](https://github.com/ikenga-hq/ikenga/releases) instead. To build from
source, see Quickstart below.

## Quickstart

Prereqs:

- macOS, Linux, or Windows (Linux needs WebKit2GTK 4.1 — `libwebkit2gtk-4.1-dev`)
- Rust ≥ 1.77
- `bun` ≥ 1.1
- `claude` on `$PATH` (for the default engine + terminal panel)

```bash
cd shell
bun install
bun run tauri dev
```

The Project dashboard (⌘1) is built on the **Home** canvas — a free-form workspace of
built-in widgets (greeting, quick actions, scratchpad) and pkg-contributed ones (tasks,
inbox triage, boards, finance), with a daily-address summary row above it. **Customize**
turns on edit mode: drag widgets in from the palette and reposition them, and the layout
persists. Navigate via the activity bar, sidebar, and command palette (⌘K / Ctrl+K). The
first-run onboarding flow handles account / engine setup; no env file is required to launch.

### Optional env vars

No env vars are needed to start the app — connections are configured at runtime
through onboarding. For dev convenience you can pre-seed values in `.env.local`
(see `.env.example`):

| Var | Purpose |
|---|---|
| `VITE_SUPABASE_URL` | Supabase project URL |
| `VITE_SUPABASE_ANON_KEY` | Supabase anon key |
| `VITE_DEV_FORCE_STUB_AUTH` | Dev-only — bypass real auth |
| `VITE_SUPABASE_USER_JWT` | Dev-only — pre-issued user JWT for impersonation |

## How pkgs work

Each pkg is a self-contained directory: a `manifest.json` plus whatever it contributes. The
manifest is validated against the Zod schema in
[`@ikenga/contract`](https://github.com/ikenga-hq/ikenga-contract); the kernel registers
each declared block against the matching registry. UI pkgs mount as **iframe** (the common
case), **webview** (for sites that block iframe embedding), or **component** (built-ins
only).

Develop with a live-reload loop — no shell restart:

```bash
ikenga dev /path/to/your/pkg   # symlink-mount + watch; reload on save
```

<!-- GIF: docs/media/pkg-dev.gif — `ikenga dev` mounting a pkg, then an edit-on-save triggering an in-place reload in the running shell -->

First-party pkgs live in the [`ikenga-pkgs`](https://github.com/ikenga-hq/ikenga-pkgs)
monorepo — read those for working examples of each archetype.

## Multi-engine chat

The chat layer is multi-engine. The frontend sees one wire — ACP-shaped session updates —
regardless of which engine backs a thread. Engine and model are selectable per turn. Every
engine below except OpenRouter (an HTTP engine) needs its CLI on `$PATH`.

| Engine | Maturity | Auth |
|---|---|---|
| **Claude Code** | Supported (default) | `claude login` / `ANTHROPIC_API_KEY` |
| **Codex** | Beta | `codex login` / `OPENAI_API_KEY` |
| **OpenRouter** | Beta — cannot resume a session after a restart | `OPENROUTER_API_KEY` |
| **Antigravity** | Beta | `GEMINI_API_KEY` (`agy` CLI) |
| **OpenCode** | Experimental | `opencode /init` |
| **Pi** | Experimental | `pi /login` |

Each engine is its own pkg under
[`ikenga-pkgs/packages/engine/`](https://github.com/ikenga-hq/ikenga-pkgs/tree/main/packages/engine).
The `cursor-agent` engine pkg is a scaffold with its runtime stubbed, so it is not a usable
engine yet.

## Scripts

```bash
bun run tauri dev    # full app (opens window, hot-reloads)
bun run dev          # Vite only — component work without the Tauri shell
bun run typecheck    # tsc --noEmit (fastest correctness check)
bun run build        # typecheck + Vite build (bundles ./dist for Tauri)
bun run tsr:generate # regenerate routeTree.gen.ts after route changes
bun run fmt          # biome format --write .
bun run lint         # biome lint .
bun run test         # vitest run
bun run engine:smoke # probe engine CLI wire protocols (no build needed)
```

`dev` and `build` first run bundle prereqs automatically (`bun:fetch`,
`iyke:bundle`, `artifact:bundle`, `iyke:mcp:build`) — no manual step needed.

## Layout

```
src/
├─ routes/                # file-based routes (TanStack Router)
│  ├─ __root.tsx          # mounts the workspace shell
│  ├─ index.tsx           # /  → Home canvas (also mounted at project/dashboard)
│  ├─ artifacts/  claude/  packages.tsx  pkg/  projects/
│  ├─ sessions/  settings/  onboarding/  scratchpads.tsx  todos.tsx
│  └─ …
├─ shell/
│  ├─ workspace.tsx       # PanelGroup root + persisted sizes
│  ├─ activity-bar.tsx    # left activity rail
│  ├─ sidebar.tsx         # mode-aware sidebar
│  ├─ content-pane.tsx    # renders <Outlet />
│  ├─ command-palette.tsx # ⌘K / Ctrl+K
│  ├─ home/               # Home widget canvas
│  ├─ panes/              # side-pane tabs (Terminal | Chat | Viewer | Off)
│  ├─ onboarding/         # first-run + engine auth wizard
│  └─ native-menu.ts      # Mac-only menu bar
├─ components/ui/         # shadcn primitives
├─ lib/
│  ├─ tauri-cmd.ts        # UI↔OS contract (matching Rust cmds in src-tauri/)
│  ├─ supabase.ts  query-client.ts  query-keys.ts
│  └─ …
└─ main.tsx
```

The Rust core lives in `src-tauri/` (Tauri commands, PTYs, pkg kernel + sidecar
supervisor, iyke control bridge). Everything the UI needs from the OS goes
through `src/lib/tauri-cmd.ts` — never call `invoke()` directly from components.

## Keyboard shortcuts

| Shortcut | Action |
|---|---|
| ⌘K / Ctrl+K | Command palette |
| ⌘B / Ctrl+B | Toggle sidebar |
| ⌘\ / Ctrl+\ | Toggle side pane |
| ⌘T / Ctrl+T | New terminal |
| ⌘⇧T | New chat |
| ⌘R | Resume last session |

Native menu is wired Mac-only; on Linux/Windows the same items are reachable via
the command palette. A **global summon shortcut** is registered on startup
(macOS ⌥Space, Linux Super+Space) — press it from anywhere to show / hide +
focus the Ikenga window.

## Status

Released builds are on the [Releases page](https://github.com/ikenga-hq/ikenga/releases);
the version badge at the top shows the latest release.

## Distribution

Two release paths: **GitHub Actions** for cross-platform release builds, and
**local build + install scripts** for fast single-platform iteration.

### GitHub Actions (recommended for releases)

`.github/workflows/release.yml` runs on tag push (`v*`) and produces installers
for all three targets in parallel:

| Runner | Target | Output |
|---|---|---|
| `macos-latest` | `universal-apple-darwin` | `.dmg`, `.app.tar.gz` (one universal build: Apple Silicon + Intel) |
| `windows-latest` | `x86_64-pc-windows-msvc` | NSIS `.exe` |
| `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | `.deb` (`.AppImage` — see caveat) |

```bash
git tag v0.0.1
git push origin v0.0.1   # → draft release on the Releases page
```

**Manual test build** (no tag): Actions tab → Release workflow → Run workflow.
Pick a single platform (`linux-only` / `mac-only` / `windows-only`; default `all`) to
test fast. Artifacts attach to the run; no release is published.

**Setup:** workspace sibling deps (`@ikenga/contract`, `@ikenga/tokens`, the
engine + mcp-iyke pkgs) are public Apache-2.0, so the default `GITHUB_TOKEN`
works out of the box. Only if you fork a sibling repo private do you need a
fine-grained PAT with read access to it, added as the `WORKSPACE_DEPS_PAT`
secret.

Caveats:
- macOS builds are **unsigned + un-notarized** (no Apple Developer ID). Users
  see Gatekeeper warnings; right-click → Open on first launch.
- Windows builds are **unsigned**. SmartScreen warns on first run.
- Linux `.AppImage` may fail because `linuxdeploy` runs `ldd` on the bundled
  bun-compiled binaries, which `ldd` can't parse. The `.deb` always succeeds.

### Local install (single platform)

Unsigned, no notarization, no remote distribution.

#### Linux (Pop_OS! / Ubuntu)

```bash
cd shell
bunx tauri build --target x86_64-unknown-linux-gnu
./scripts/install-linux.sh
```

Wraps `sudo dpkg -i` against the produced `.deb`. Binary lands at
`/usr/bin/ikenga-desktop`, `.desktop` entry under `/usr/share/applications/`.
Idempotent re-run; uninstall with `sudo dpkg -r ikenga-desktop`. Requires
WebKit2GTK 4.1 (`libwebkit2gtk-4.1-0` at runtime — Pop_OS! 22.04+ / Ubuntu
24.04+ ship it). If the webview crashes on startup with HW-accel issues:

```bash
WEBKIT_DISABLE_COMPOSITING_MODE=1 ikenga-desktop
```

#### macOS (Apple Silicon)

> ⚠️ For releases, prefer GitHub Actions above. This local path is for fast
> iteration on a Mac dev machine.

```bash
cd shell
bunx tauri build --target aarch64-apple-darwin
./scripts/install-mac.sh
```

Tauri produces an ad-hoc-signed `.app`; the script copies it to
`/Applications/Ikenga.app` and strips the quarantine xattr so Gatekeeper
doesn't block first launch. For a universal binary, set
`TAURI_TARGET=universal-apple-darwin` and build that target instead.

### Server Deployment & Task 0 Credential Bootstrap

`ikenga-server` is a headless control plane binary that can run on a remote server or VM (behind a Tailscale perimeter).

#### Build & Stage Artifacts
Run `shell/scripts/server/deploy.sh` from a clean checkout of `shell`:
```bash
./scripts/server/deploy.sh
# Stages compiled binary & frontend assets into scripts/server/out/

# Deploying from a non-Linux machine? Name the server's target, or you will
# stage a binary the host cannot execute:
TARGET=x86_64-unknown-linux-gnu ./scripts/server/deploy.sh
```

#### Task 0 Credential Bootstrap
On the target host, run `bootstrap-credentials.sh` prior to starting `ikenga-server`:
```bash
./scripts/server/bootstrap-credentials.sh
```
This idempotently generates server secrets into `/opt/ikenga/.env` (mode 600). It only
ever *appends* — an existing value is left alone, so re-running it can never destroy a
credential nobody has another copy of.

- `IKENGA_AUTH_TOKEN`: Pinned bearer token for API + WebSocket authentication. Read by
  `ikenga-server` directly (clap `env = "IKENGA_AUTH_TOKEN"`).
- `IKENGA_VAULT_KEY`: **Reserved — nothing reads it yet.** The headless vault is WP-12b
  (ikenga#100); `secrets_set` currently answers "not implemented in the headless daemon".
  It is generated now so the vault lands on a key that has been stable since first boot.
- `ANTHROPIC_API_KEY` / `GEMINI_API_KEY` (the latter is used by the Antigravity engine): appended to the same env file if exported.
  systemd loads it via `EnvironmentFile` and the engine adapters inherit their
  environment, so the agent CLIs pick them up without any config file being written.

> ⚠️ **Security Posture Note (G-30)**: keeping long-lived credentials in
> `/opt/ikenga/.env` lets `ikenga-server` run headless without manual passphrase entry.
> That is the trade: **box compromise equals credential compromise**. It applies today to
> `IKENGA_AUTH_TOKEN` and the agent API keys; `IKENGA_VAULT_KEY` joins them once WP-12b
> gives it a consumer.

#### Verifying a running daemon

`scripts/server/verify-live.ts` drives the daemon's real HTTP and WebSocket
surface — real PTYs, real reconnects, and a real `agy` turn. Nothing in it is
stubbed, which is the point: the two worst defects found in this subsystem (a
`tokio::join!` that never returned, and a deleted stylesheet import) both
compiled clean and passed every offline gate.

```bash
IKENGA_VERIFY_URL=http://127.0.0.1:4477 IKENGA_AUTH_TOKEN=<token> \
  bun run scripts/server/verify-live.ts
```

#### Running under systemd or Docker
- **systemd**: Copy `scripts/server/ikenga-server.service` to `/etc/systemd/system/ikenga-server.service` and run `systemctl daemon-reload && systemctl enable --now ikenga-server`.
- **systemd, multi-user (T1)**: `scripts/server/ikenga-server-t1.service` instead. [`scripts/server/README.md`](scripts/server/README.md) covers the unit, local accounts, and migrating a T0 install with `ikenga-server accounts adopt-t0`.
- **Docker**: Run `docker compose -f scripts/server/docker-compose.yml up -d` after running `deploy.sh`.

### What we don't do yet

- No code signing (no Apple Developer ID, no Windows EV cert)
- No notarization
- No Snap, Flatpak, or Mac App Store
- No system-tray icon (global shortcut covers summon UX)

GitHub Releases (via the Actions workflow) is the supported remote distribution
channel. The built-in updater checks GitHub Releases (`latest.json`) for new app
versions, while packages update from the registry at
`https://registry.ikenga.dev/index.json`; one update flow covers both, and it shipped
in v0.14.0. Revisit signing / notarization if Ikenga ever needs to be installed at scale.

## Links

- [ikenga.dev](https://ikenga.dev) — site + docs
- [Documentation](https://ikenga.dev/docs)
- [`ikenga-pkgs`](https://github.com/ikenga-hq/ikenga-pkgs) — first-party pkg monorepo (working examples per archetype)
- [`ikenga-contract`](https://github.com/ikenga-hq/ikenga-contract) — manifest schema, RPC types, capability scopes
- [`CLAUDE.md`](CLAUDE.md) — detailed architecture for contributors
- [Building in the Loop](https://buildingintheloop.substack.com/subscribe) — biweekly letters on running a real multi-agent Claude Code system

## License

Apache-2.0 — both the platform and the first-party pkgs. See [`LICENSE`](LICENSE).

## Contributing & community

Issues and PRs welcome — see [`.github/CONTRIBUTING.md`](.github/CONTRIBUTING.md). Start a
thread in [Discussions](https://github.com/ikenga-hq/ikenga/discussions); report security
issues per [`.github/SECURITY.md`](.github/SECURITY.md). For workflow deep-dives and
build-in-public notes from the founder, subscribe to
[Building in the Loop](https://buildingintheloop.substack.com/subscribe) — biweekly.
