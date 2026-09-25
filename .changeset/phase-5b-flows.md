---
"ikenga-desktop": minor
---

**Phase 5b: flows, viewer and pane chrome.**

- **Onboarding (D-04).** Consecration is rebuilt as seven steps: welcome, engine, project, equipment, look, shortcuts, done. It has a step rail, resume/offline/engine-none states, and a Personal/Project scope switch. It never scaffolds into `~/.claude/` implicitly.
- **Daily address.** It sits on the Project dashboard. It shows project-scoped runs and unresolved permissions, is dismissible once a day, and has a real Workspace setting.
- **Notifications.** A notifications table (`shell_notifications`, migration 0066) records permission, run, update and violation rows. Rows keep a resolved state that is separate from read. Terminal prompts resolve on `Stop`, `PostToolUseFailure` and the next `UserPromptSubmit`. Per-kind mutes live in `settings.json`. The status-bar bell and a notification centre show the rows, toasts are text-only copies of them, and the iyke bridge serves `GET /iyke/notifications`.
- **Updater (D-07).** One updater flow covers the app and packages: release notes, status-bar progress, restart with a live-session warning, and a package batch that holds any update requesting a new permission for review *before* installing it.
- **Automations and restore (D-07).** There is a `/automations` view. A restore wizard covers the whole restore, from picking the file to done, and lists secret names only. Shared Empty, Loading, Error and Offline states each offer exactly one next action.
- **Viewer and panes (D-08).** The artifact viewer gets new chrome, variants, and CSV and JSON renderers. Package panes get states for loading, consent, crashed, sidecar-down and blocked, with a working "Allow host…". Native menu parity: the macOS tree, plus a `≡` cascade on Windows/Linux, with keys taken from the keymap registry.
- **New commands:** `notifications_*`, `pkg_trust_preview_incoming` and `secrets_index_names`. Each has its ACL entry.
