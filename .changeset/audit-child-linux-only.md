---
"ikenga-desktop": patch
---

macOS and Windows desktop builds compile again: the T1 audit reconcile loop (`access::audit::child::spawn`) is Linux-only, like the broker it serves. Same contents as 0.24.0, whose desktop builds failed.
