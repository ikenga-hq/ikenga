---
"ikenga-desktop": patch
---

Fix the PDF viewer's **Download** button in a browser session. It did nothing (or saved an empty file) once the PDF had rendered, because the PDF renderer had taken the loaded bytes for itself. Downloads now save the full file.
