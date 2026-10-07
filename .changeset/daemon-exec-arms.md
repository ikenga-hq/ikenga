---
"ikenga-desktop": patch
---

Browser sessions now get five things that were desktop-only: the title-row git branch chip and the Explorer's git-status badges (`pkg_sidecar_call`), project and personal shell actions (`action_exec`), pin routing to a terminal, Chi or the clipboard (`comment_route`), the schedule table's "Run now" (`agent_ops_run_now`), and pkg settings edits (`pkg_settings_set`). On a multi-user server each of these runs as the signed-in person and stays inside their own files. A sidecar runs only from its own pkg's folder. An action or pin runs only in a folder the server's allowlist covers. "Run now" fires only your own jobs, through your own agent-ops daemon. Settings writes go to your own database, and only for keys the pkg declares. Enabling, uninstalling or restarting a pkg is still desktop-only, because the server has no pkg kernel.
