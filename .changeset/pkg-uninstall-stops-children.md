---
"ikenga-desktop": patch
---

Uninstalling or reinstalling a pkg now stops its running processes first.

- Uninstall stops the pkg's long-lived MCP server or supervised sidecar and waits for it to exit before it moves the pkg's folder. The wait is bounded. A process that ignores the stop is killed; on Windows its whole process tree is killed. Before this fix, a running process could keep the folder locked, so the uninstall only half-completed and a reinstall then failed with "os error 32".
- Reinstalling from the registry stops the running copy of the pkg before replacing its folder. The folder move retries briefly. If the folder is still locked, the error now names the holder, for example "a Meetings process is still running (pid N)".
- A pkg that is parked or uninstalled while its MCP server is still starting is now stopped. Before, it was marked running and left orphaned.
- Project reconcile never parks a workspace-scoped pkg. It also no longer re-registers a pkg that was freshly installed and is already running.
