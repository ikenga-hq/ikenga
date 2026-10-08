---
"ikenga-desktop": minor
---

`provision.sh tunnels`: SSH tunnels as hardened systemd units on a server box (runs before `backups` in the full provision run). A profile `TUNNELS` list gets one `<name>-tunnel.service` each, run by a single `ikenga-tunnel` system user with one ed25519 key per box (generated once, never regenerated) and a `known_hosts` written only from the profile's pinned `TUNNEL_KNOWN_HOSTS` (no trust-on-first-use; a tunnel without a pinned key is refused). Every profile field is strictly validated, a symlink or foreign owner anywhere in the tunnel user's tree is refused, and root only touches that tree as the tunnel user. Each run prints the exact `restrict,port-forwarding,permitopen=...,from=...` authorized_keys line to install on the remote host. An existing hand-made tunnel unit with the same effect is adopted unchanged (no rewrite, no restart); a tunnel removed from the profile has its unit stopped and removed and its key kept.
