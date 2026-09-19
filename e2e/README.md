# Frame test harnesses

Phase 1 of the shell UX rearchitecture is checked by **two harnesses** (DEC-27). No
check drives the production Tauri webview: macOS has no WebDriver for WKWebView and
`tauri-driver` only runs on Windows and Linux, so a real-app e2e suite could not
cover all three platforms.

| | Browser mode (`bun run test:e2e`) | Live probe (`bun run probe:frame`) |
|---|---|---|
| Drives | the React frame in headless Chromium | the real running shell, over the iyke localhost bridge |
| Host | mocked at the Tauri IPC seam (`e2e/fixtures/tauri-mock.ts`) | real: Rust, SQLite, PTYs, Chi runtime |
| Runs | locally and in CI (`ci.yml` job `e2e`, Linux) | locally, against `bun run tauri dev` |
| Files | `playwright.config.ts`, `e2e/*.spec.ts` | `scripts/frame-live-probe.ts` |
| Result | Playwright pass / fail | `PASS` / `BLOCKED` / `FAIL` per check; exit 1 only on `FAIL` |

## Which harness a check goes in

A check goes in **browser mode** if the frame can show it without a real host.
It goes in the **live probe** if a mock could make it pass while the real thing is broken.

**Browser mode**
- rail inventory: which items render, in what order, and with which labels
- Explorer sections
- the merged title/tab row
- splitting to 6 panes, and focusing a pane by index
- the command palette
- the Shortcuts view
- focus rings and keyboard reachability
- contrast (axe or computed styles against the token palette)
- hit-target sizes

**Live probe**
- project-switch propagation: the active project is persisted in Rust, the
  `projects:active-changed` event reaches the frontend, and the rail re-renders.
  Once WP-21 lands, `iyke state` also has to report the new project.
- PTY dispatch: text sent into a real terminal runs, and its output comes back
- `chi_run`: a real agent run starts and finishes (opt-in with `--with-chi`)
- real v15 → v16 migration on a copy of a v15 profile (`v15-migration`; still a
  BLOCKED stub; WP-02 has merged the migration, the probe check itself is a follow-up)

If a check needs both halves, split it. For example, "switching project re-titles
the Explorer" is a browser-mode spec. It drives the switch with
`emitHostEvent(page, 'projects:active-changed', …)`. The live probe then proves
that the real host emits that event.

## Browser mode

```bash
bun run test:e2e                          # vite build + vite preview on :14210, then Playwright
IKENGA_E2E_REUSE_BUILD=1 bun run test:e2e # skip the rebuild when only specs changed
bun run test:e2e -- --headed              # watch it
```

- The server is `vite build` followed by `vite preview`, using the repo's own
  `vite.config.ts`. It is not `vite dev`, because the dev server serves about
  2,300 unbundled modules and re-optimises `@codemirror/*` late. On a cold load
  that leaves requests hanging and the frame never mounts. None of the
  `bun run dev` pre-steps run, because they build Tauri bundle resources and the
  renderer does not import any of them. The build output, traces and failure
  screenshots go to `node_modules/.cache/` (already gitignored).
- The port is not 1420, so the harness can run while a dev shell is up. Override
  it with `IKENGA_E2E_PORT`.
- Browsers: run `bunx playwright install chromium` once per `@playwright/test`
  version.

### The Tauri mock

`installTauriMock(page, { responses })` must run before `page.goto`. It installs
a fake `window.__TAURI_INTERNALS__`, so `isTauri()` is true and the real
`TauriTransport` → `@tauri-apps/api` path runs unchanged. Every `@/lib/tauri-cmd`
call reaches the mock as a command name, such as `project_list`,
`activity_pins_list`, `pkg_kernel_status` or `pty_spawn`. No product code or
bundler alias is involved.

- `DEFAULT_RESPONSES` is the smallest host that boots today's frame: two
  projects, one pinned route in one section, one pkg rail entry, completed
  onboarding and empty SQLite. Override a command for one spec with
  `responses: { cmd: value }`. To make a command reject, use `{ __error: 'msg' }`.
- Commands with no canned answer resolve to `null` and are recorded.
  `unmockedCommands(page)` lists them, and the smoke spec attaches that list as
  a test annotation.
- `emitHostEvent(page, event, payload)` sends a host event, the way Rust
  `app.emit` would.
- Requests to any host other than the preview server are aborted, so the frame
  renders offline. Pass `allowExternal: true` to change that.
- Assert on `pageerror` (uncaught exceptions and unhandled rejections), not on
  console errors. Several boot paths log and carry on by design when a host
  service is missing.

### Specs

- `frame.spec.ts`: a smoke test of the frame as it stands on
  `feat/phase-1-frame`, after WP-02's G-STATE v16 store and before WP-03/WP-04
  rework the rail and sidebar. It checks that the core rail items (by
  accessible name), the pkg entry, the pin and its section, the project
  indicator, a sidebar region and the pane tree all render, that selecting
  Settings on the rail shows the settings navigation, and that no uncaught
  errors occur. It deliberately asserts no mode-specific sidebar title: since
  v16, the old rail's Files item stores mode `project` (g-state.md §6 interim
  behaviour), so those titles are in flux until WP-04. WP-20's no-op slot
  refactor has to keep it green without editing it.

Each later WP adds `e2e/<area>.spec.ts` for its own browser-mode DoD lines.

## Live probe

```bash
bun run tauri dev                  # in another terminal
bun run probe:frame                # every check
bun run probe:frame -- --read-only # skip checks that change state (project switch, PTY)
bun run probe:frame -- --with-chi  # also start a real chi_run
bun run probe:frame -- --only=bridge,pty-dispatch
```

The probe finds the bridge through `control.json` in the app's local data dir.
That is `%LOCALAPPDATA%\app.ikenga\` on Windows,
`~/Library/Application Support/app.ikenga/` on macOS and
`~/.local/share/app.ikenga/` on Linux. It uses the same lookup as
`iyke-cli/src/control.rs`. You can override it with `--control=<path>`, or with
`IKENGA_IYKE_URL` plus `IKENGA_IYKE_TOKEN`. It calls the bridge over plain HTTP,
so no `iyke` binary is needed, and it never prints the token.

- **BLOCKED** means a prerequisite is missing. The causes are: no shell running
  (including a stale `control.json` left by a crashed shell), fewer than two
  projects to switch between, an opt-in flag that was not given, or a producer
  WP that has not merged yet. BLOCKED is not a regression and exits 0.
- **FAIL** means the shell answered and the answer was wrong. It exits 1.
- Checks that change state clean up after themselves. The project switch
  restores the original active project. The PTY check kills its terminal and
  closes the terminal's tab.

When a WP adds a host round-trip that a mock cannot prove, it adds a check to
`CHECKS` / `RUNNERS` in `scripts/frame-live-probe.ts`. Until its producer merges,
the check reports BLOCKED with the name of the WP it is waiting on.
