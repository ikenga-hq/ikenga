---
"ikenga-desktop": patch
---

Fix pkg views (the Outbox approvals page first) failing inside a shared project. In a share an Operator got "Can't read the outbox queue here … HTTP RPC error: 403": before it serves any `host.*` verb the pkg host reads the pkg's manifest through `pkg_preview_manifest`, and that call was owner-only, so even the share-routed `host.paActions.list` was refused. `pkg_preview_manifest` is now share-reachable (same `files` capability). An installed pkg answers by exact path, since its manifest is operator-installed and non-secret; any other path is still confined to the shared project. Raw `db_query` / `db_exec` stay owner-only.
