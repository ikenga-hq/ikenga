---
"ikenga-desktop": minor
---

Servers with accounts can now be set up to let people share one of their own files with someone who has no account, by an outside link. This release adds the groundwork only: where links are kept, the setting that turns them on (`--file-links` / `IKENGA_FILE_LINKS`, off by default, and `FILE_LINKS=1` in the provisioner for public HTTPS hosts), and the requests to create, list and revoke links, including an administrator view of every account's links. The public link page and the controls in the app come in a later release, so nothing changes for anyone until both have shipped and the setting is turned on.
