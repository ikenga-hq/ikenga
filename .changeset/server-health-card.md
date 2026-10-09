---
"ikenga-desktop": minor
---

Server health for browser sessions. Admins (the T1 admin, or the T0 owner) get a "Server health" card in Settings > About: memory, swap, memory/CPU/IO pressure, disk, load, the backup job's per-database status, the state of the backup timers and database tunnels, and per-account terminal and `claude` process counts, with amber/red checks (memory < 15% available or tight with no swap, disk < 10% free, memory pressure avg60 > 10, any failed backup, timer or tunnel). It is served by the new admin-only `server_health` RPC (cached ~5 s, bounded, every field optional). Every browser viewer also gets a connection indicator: the round trip to the server as median and jitter ("340 ms ± 85") on the Health page and in the status bar, amber past 150 ms and red past 300 ms, measured with a tiny ping/pong on the events socket and paused while the tab is hidden. The desktop shows neither.
