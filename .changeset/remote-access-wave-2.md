---
'ikenga-desktop': minor
---

Remote-access wave 2: the headless server surface, plus two terminal fixes and
a credential-leak fix.

**Security — the daemon no longer leaks its own credentials into every PTY.**
`pty::spawn_inner` inherited the parent environment wholesale, and
`bin/ikenga-server.rs` has clap read `IKENGA_AUTH_TOKEN` from the environment,
which systemd populates from `/opt/ikenga/.env`. Anyone who could open one
terminal could read the bearer token that grants terminals — a credential that
outlives the session and survives revoking the client. Privilege *persistence*
rather than escalation (holding the token already implies shell access), which
is why this ships as a fix.

Two layers: `ikenga-server` scrubs `IKENGA_AUTH_TOKEN` / `IKENGA_VAULT_KEY`
from its own environment once clap has read them, and `pty::is_host_only_env`
denylists those plus `IKENGA_SECRET_*`. The `env_clear()` before the inherit
loop is load-bearing and not obvious: `CommandBuilder` seeds itself from
`std::env::vars_os()` at construction, so it inherits by default and skipping a
key in the loop leaves the already-inherited copy in place. Agent credentials
(`ANTHROPIC_API_KEY` and friends) are deliberately still inherited — a remote
terminal holding the box's agent auth is the point of the design. The split is:
unprefixed env reaches agent CLIs, `IKENGA_*` host credentials never reach a
shell.

**Terminal labels can no longer collide.** `/iyke/terminal/spawn` rejected a
duplicate label by scanning for `status == "running"`, which cannot see a
terminal that has been spawned but has not yet reached `running` — so two
concurrent spawns with the same label both passed, roughly 1 in 5. Replaced
with `PtyManager::reserve_label`, which takes the name under a lock and holds
it until `set_label` succeeds; `LabelReservation`'s `Drop` releases it on every
early return so a failed spawn doesn't strand the name.

**Popped-out terminals no longer fight over the PTY size.** Two windows
attached to one PTY at different sizes drove conflicting resizes and corrupted
the reflow. Attached non-owning viewers now skip `ptyResize` while their window
is unfocused (active viewer wins), and an unchanged size is a no-op on the Rust
side.

Also lands the headless server surface behind it: static pkg serving with
lexical-then-canonical traversal checks, the fs-socket transport, and the
symlink-escape fix in the watcher allowlist.
