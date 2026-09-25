---
"ikenga-desktop": minor
---

**Phase 5a: settings shell + secrets unlock.** Settings is rebuilt on the
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