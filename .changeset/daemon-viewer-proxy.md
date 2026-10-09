---
"ikenga-desktop": patch
---

Serve authenticated HTML, audio, and video viewer previews on the daemon (`/__viewer/<token>/…`) with Range requests, a `sandbox` CSP (no `allow-same-origin`), and bridge injection. The file served is the exact canonical path that passed the symlink, allowlist and reserved-directory checks (never re-resolved from a URI), mounts expire when idle, and under multi-user mode the broker routes the token's principal to its already-running child without a session and never launches one. In browser sessions previews are in-app only: "Open in browser" / "Copy viewer URL" stay hidden, and the viewer mount is stopped (`viewer_stop`) when the preview pane unmounts or switches file.
