---
"ikenga-desktop": patch
---

Server: the app lock now works per account on the daemon, and the server enforces it rather than only showing a lock screen. While an account is locked, everything but checking the lock, unlocking and the box's health is refused: terminals pause and show nothing (not even scrollback), and repaint cleanly on unlock; anything typed or sent to a chat behind the lock screen is dropped instead of running later; no new terminal can be started; file watching goes quiet and file previews, backups and device or sharing changes are refused. Unlocking is rate-limited, and abandoning an unlock attempt part-way no longer lets a second check start, so it can't be used to exhaust the server's memory.
