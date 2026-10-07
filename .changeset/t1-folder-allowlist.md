---
"ikenga-desktop": patch
---

Browser sessions can now open folders. On a multi-user (T1) server each person's folder list starts with their home folder: new accounts get it on first sign-in, and existing accounts with an empty list get it too, while a list someone emptied on purpose stays empty. People can add, remove and reset their own folders under Settings → Storage → "Folders you can open", and admins can change anyone's list by username. A single-user (T0) server keeps an empty list until the owner adds a folder, which now also works from the browser. The folder picker no longer falls back to the server's working directory or saves `.` as a project. With no folders it offers "Add a folder", or tells people who to ask when they can't add one themselves.
