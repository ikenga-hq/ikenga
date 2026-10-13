---
"ikenga-desktop": patch
---

Server: pkg tool calls, MCP calls and sidecar calls now work from a browser session. Each account runs its own sidecars under its own user, capped at 8 per account, and they are stopped with their whole process group when idle or on shutdown. A pkg runs for an account only when an admin has assigned it and approved its trust for that exact version and permission set. Built-in `com.ikenga.*` pkgs count as built-in only when they sit inside the configured pkgs folder. The T1 broker again passes the request host through, and the T0 owner's own devices can use assigned third-party pkgs.
