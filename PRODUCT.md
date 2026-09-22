# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

Primary: a solo developer who lives in Claude Code every day, on one machine, running several agent sessions across a few projects. They read what the agent produced, approve permission requests, dispatch the next instruction, and keep their agent setup (skills, agents, hooks, MCP tools, apps) in order. Confirmed 2026-09-16.

Music-label and operator work (mail, tasks, finance, content pkgs) is one project among many and enters through pkgs and scaffolding presets, not through the chrome. It is not the audience.

## Product Purpose

Ikenga is the open-source desktop home for a Claude Code setup: a Tauri 2 shell that hosts terminals running coding agents ("Chi"), installable packages (apps, engines, MCP tools, sidecars, skills), HTML artifacts, and the project-scoped configuration those agents read. Success is daily use as the place where the developer's agent work is visible and manageable, with the CLI (`iyke`) and MCP as first-class equals to the GUI.

## Positioning

A visible, composable home for your agent setup: skills, agents, commands, hooks, tools, apps, workflows, schedules and artifacts are objects you can see, install, scope, and edit, and agents and humans edit the same files on disk. Multi-engine by design (Claude Code default; Codex, Gemini, others as engine pkgs). No chat UI: the terminal is the conversation and the shell shows state around it (ADR-019). Confirmed 2026-09-16.

## Operating Context

- One window, several panes, one or more terminals with `claude` (or another engine CLI) running; one active project at a time, switched often.
- Objects live on disk: `~/.claude/*` and `<project>/.claude/*` for primitives, `ikenga-pkgs`/registry for pkgs, project folders for artifacts, SQLite for shell state.
- Agents create most things (via terminal + prompts, `ikenga dev`, skills like `ikenga-pkg-builder`, `groundwork`, `ikenga-artifact-builder`); humans review, hand-edit, promote, and wire into automation.
- The runtime controller `iyke` and the `mcp-iyke` MCP server drive the running shell from outside; anything the GUI shows must also be readable there.
- Lore vocabulary is bounded (design/shell/05-lore-and-nomenclature.md): Ikenga, Obi, Chi, Iyke, consecration, daily address, share kola are the only Igbo surface words; Ngwa ("equipment") is in use on the rail and keeps a first-contact gloss. Skills/agents/commands/hooks stay English.

## Capabilities and Constraints

- Tauri 2 + Vite + React 19; Rust core owns PTYs, pkg kernel, sidecar supervisor, viewer server, iyke bridge, child webviews. Linux child-webview positioning caveats accepted.
- Windows, macOS, Linux at parity. Confirmed.
- Keyboard-first: ⌘1/⌘2/⌘3 rail modes, ⌘K palette, ⌘P project switch, ⌘T new tab, ⌘N new object; every rail and pane action reachable without a mouse. Confirmed.
- Offline / local-first: everything works without network except registry fetches and engine APIs. Confirmed.
- Pkg manifest schema v4 today; v5 (contributed views, explorer sections, companion panels, context actions, widgets, workflows) is planned and will migrate all 16 apps in one PR.
- Trust model: signed registry index; sensitive permissions require consent; skill-only pkgs declare intent, never grant.
- Undecided: whether a pkg may request a rail pin beyond one-time `pin_on_install`; workflow interchange format details beyond "contract view-model + importers".

## Brand Commitments

- Name Ikenga; CLIs `ikenga` (pkg manager) and `iyke` (runtime controller) are fixed.
- Visual world is locked and lives in `@ikenga/tokens` (`tokens/tokens.css`): Theme A Dusk Wood default, B Kola Daylight, C Bronze Shrine; dark default; density compact/comfortable/spacious. Fraunces for display moments only, Inter body, JetBrains Mono for code. Lucide functional icons.
- Materiality over glass; chip-carve geometry sparingly; no literal masks or figurines; cultural attribution in About. Guardrails in design/shell/04-ikenga-direction.md and design/BRAND-STRATEGY.md.

## Evidence on Hand

- Real shipped surfaces: shell v0.9.0, 17 app pkgs, 8 engines, 21 skills, 4 MCP servers, 4 sidecars in `ikenga-pkgs/`; registry with 36 entries.
- Real sample data for mockups: the `royalti-co` monorepo (branches, files, artifacts such as `dashboards/royalti-pulse/index.html`).
- Design system audits: plans/shell-design-system (28 chrome parts locked), plans/cockpit Round 2 critique (G-01..G-04), plans/ngwa-design-system.
- No testimonials, customer logos, usage benchmarks, or pricing exist. Do not fabricate them.

## Product Principles

1. Project is the container: every surface is scoped to the active project.
2. The agent's work is visible, never hidden in a chat: state, cost, permissions, and outputs are first-class.
3. Everything is a file the human can open and edit, and an object the agent can create; both paths stay in sync.
4. Compose, don't silo: packages contribute into shared surfaces instead of owning modes.
5. CLI, MCP, and GUI are equals; nothing exists only in the GUI.

## Accessibility & Inclusion

WCAG 2.2 AA for the chrome: measured contrast, visible focus rings, `prefers-reduced-motion` respected, full keyboard operation. Confirmed 2026-09-16.
