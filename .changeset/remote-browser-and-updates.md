---
"ikenga-desktop": minor
---

Remote server and browser:

- **In-app server updates:** admins see when a new stable server release is available (banner + Settings › Server) and can update from the browser, with the open-terminal count and a health check with rollback (notify-only by default).
- **Installable app + push:** the browser client is an installable PWA (cached shell, live data) with push notifications for permission requests, finished/failed runs, available updates (admins) and invites/pairing.
- **Phone dispatch** sends only to agent CLIs or as a Chi follow-up, never into a plain shell.
- **Clipboard and terminal in the browser:** copy works on plain-HTTP origins or says it couldn't; the paste hint shows the real paste key; the terminal right-click menu no longer closes the moment it opens; Ctrl+Shift+C no longer opens DevTools; OSC 52 copies offer a Copy button when blocked.
- **Browser parity:** browser-reserved shortcuts get alternatives and file drops are handled; "Open in default app" downloads instead of opening a junk tab; notifications are requested from a click and fall back to a toast (no more phone crash); the app menu shows only what works; pkg links and downloads go through the host; the VS Code keybindings import reads a local file; a shared `toast()` utility.
