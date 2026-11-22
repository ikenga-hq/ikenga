---
"ikenga-desktop": patch
---

Checking an outside link without fetching it no longer counts as someone opening it. A HEAD request, which link scanners and chat previews often send, used to raise the open and download counts, write an "opened" entry in the activity log and send you a "Someone opened your link" notification even though no file was sent. It now gets the same answer as before and records nothing.
