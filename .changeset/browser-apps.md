---
"ikenga-desktop": patch
---

Installed apps such as Studio, Tasks and Sales now open in the remote browser client. The server reports app views and rail entries, answers the app trust and badge calls, and lets the browser load an app's own files with a short-lived cookie that only works under `/pkgs` and never contains the server token. App features that run local processes (sidecars, MCP tools, host fetch) report that they aren't available in the browser yet.
