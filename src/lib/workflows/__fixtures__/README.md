# Workflow importer fixtures (WP-31 · Round 29 review fix)

The WP-31 DoD says *each importer round-trips one **real** source*. The original
test suite only tried for that on the groundwork importer, and silently fell
back to an inline string when the real file was missing. These are copies of the
real sources, checked in so the tests are hermetic. `importers.test.ts`
`readFileSync`s each one and **fails if it is missing** — there are no
fallbacks.

| Fixture | Importer | Copied verbatim from |
|---|---|---|
| `orchestrate.workflow.js` | `groundwork` (JS path) and `claude-workflow` | `ikenga/plans/shell-ux-rearchitecture/artifact/orchestrate.workflow.js` — emitted by `groundwork orchestrate --emit-workflow` |
| `09-orchestration.md` | `groundwork` (markdown path) | `ikenga/plans/shell-ux-rearchitecture/09-orchestration.md` |
| `claude-hooks-settings.json` | `hooks` | the `hooks` block of the real user-level `~/.claude/settings.json` (a `PreToolUse` matcher running `route-subagent-model.py`) |
| `cron-manifest.json` | `cron` | `ikenga-pkgs/packages/sidecars/local-store-etl/manifest.json` **plus** a `cron[]` block |

## Why the cron fixture is not a pure copy

`grep -rn '"cron"' ikenga-pkgs/packages/**/manifest.json` returns **zero**
matches — no published pkg declares a `cron[]` block today (the same finding
`plans/atelier-parity/06-trigger-ownership.md` records: "the working cron infra
exists; nothing feeds it"). So there is no real `cron[]` entry to copy. The
fixture is the real `local-store-etl` manifest with the canonical `cron[]` block
from the authoring reference
(`.claude/skills/ikenga-pkg-builder/references/manifest-cheatsheet.md` §`cron`),
with `handler` pointed at that manifest's real sidecar. When a pkg ships a real
`cron[]`, replace this fixture with the copy.

## Hostile fixtures

`hostile-waves.workflow.js` and `hostile-waves-computed.workflow.js` are
**deliberately booby-trapped** — do not "fix" them. They pin the Round-29
security fix: the importers parse these files, they never execute them. Both set
`globalThis.__IMPORTER_PWNED_*` flags and the tests assert those flags stay
`undefined`.
