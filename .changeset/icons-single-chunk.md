---
"ikenga-desktop": patch
---

Faster start in the browser app: icons that pkgs, pins and actions name (for example `layout-dashboard`) now load from one deferred file instead of about 1,700 separate ones. The browser app used to fetch nearly all of them before first paint. Every icon name still works, and the desktop app is unchanged apart from fewer files.
