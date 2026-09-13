---
"ikenga-desktop": patch
---

Add the `host.sendToActiveSession` AppBridge verb (#127). Stop holding the kernel `live` lock across registry calls in `reconcile_for_project` so a registry panic can no longer poison it (#131). Write `cwd` for pkg MCP servers in `~/.claude.json` so relative args resolve (#128). Version PRs now open with `WORKSPACE_DEPS_PAT` so their CI isn't approval-gated (#193).
