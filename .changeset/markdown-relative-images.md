---
"ikenga-desktop": patch
---

Images referenced by a relative path in a Markdown file (for example `![](./chart.png)`) now show up in the viewer, in the desktop app and in a browser session. Images outside the document's folder are refused, and web and inline images are unchanged.
