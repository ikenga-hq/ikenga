---
"ikenga-desktop": patch
---

Claude terminals on the server now feed the browser's cost HUD and permission inbox. A `claude` launched in a browser terminal is handed the same hook and statusline settings the desktop gives it, wired to the server itself with a secret of its own per terminal (never the server token, and never visible in the process list), so the HUD shows real model, context and cost figures and the permission inbox sees Claude's asks. "Hold PreToolUse" works in the browser: Approve and Deny answer the waiting hook, once, and an unanswered ask is denied after 30 seconds, exactly as on the desktop. A server with no data folder says "Not available on this server" instead of leaving the HUD listening forever. The tool-call feed is not part of this change.
