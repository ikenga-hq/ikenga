---
"ikenga-desktop": patch
---

The provisioner now writes `IKENGA_FILE_LINKS=true`. It wrote `1`, which the server rejects at startup, so turning outside links on with `FILE_LINKS="1"` in a profile would have stopped the server from starting.
