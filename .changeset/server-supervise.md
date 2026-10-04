---
"ikenga-desktop": patch
---

`ikenga-server` gains a `supervise` subcommand for hosts without systemd, such as a container. Run `ikenga-server supervise -- <server flags>` as the container's entrypoint: the server becomes its child and is started again whenever it exits, and `SIGHUP` restarts it on purpose. Detached agent runs are never signalled, so they survive a server crash or restart the way they already do under the systemd unit, and the server picks their status back up when it returns. The supervisor also reaps orphaned processes, so a finished run never lingers as a zombie. `SIGTERM` stops the server and then the supervisor. Stopping the whole container still ends every run inside it. An end-to-end test covers a crash, a requested restart, the reap and a clean stop.
