---
"ikenga-desktop": patch
---

Audio, video and HTML previews now open files whose names contain `#`, `?`, `%` or spaces (for example `Report #2 50%.html`). Previously the name was put into the preview address unescaped and the file failed to load.
