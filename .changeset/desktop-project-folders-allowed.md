---
"ikenga-desktop": patch
---

The desktop app can open your project folders again. Since roots moved onto projects (WP-05), a project's folder and its extra roots showed in the file browser but every read was refused ("path outside allowlist"), because nothing added them to the list of folders Ikenga may open. Now creating a project or adding a root to it allows that folder, and on first launch an install whose list was never set up gets its existing project folders. Settings → Storage → Folders you can open also has a folder picker on the desktop and accepts `~` paths.
