---
"ikenga-desktop": patch
---

The desktop app can open your files again. Since roots moved onto projects (WP-05), a project's folder and its extra roots showed in the file browser but every read was refused ("path outside allowlist"), because nothing added them to the list of folders Ikenga may open.

- A list you never set up now starts with your home folder (as a multi-user server account's does), plus any project folders outside it; Reset goes back to home. A list you edited is left as it is.
- Creating a project or adding a root to it adds that folder.
- Settings → Storage → Folders you can open has a folder picker on the desktop and accepts `~` paths.
- Agents and the iyke CLI can ask for a folder: `GET /iyke/fs-roots` lists them and `POST /iyke/fs-roots/add {path, reason?}` shows you an Allow / Deny prompt; nothing is added unless you allow it. Bridge API 6.
