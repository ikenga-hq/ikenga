# Deploying `ikenga-server`

Build and stage with `deploy.sh` (see the top-level README's "Server Deployment"). This file covers the two systemd units and moving an install from T0 to T1.

| Unit | Tier | Runs as | Who signs in |
|---|---|---|---|
| `ikenga-server.service` | **T0**: single user | the `ikenga` service user | anyone holding the bearer token (`IKENGA_AUTH_TOKEN`) |
| `ikenga-server-t1.service` | **T1**: a team, one Unix uid per person | root (the broker); each person's child runs as their own uid | local accounts, username + password at `/auth/login` |

Install **one** of the two. Both bind port 4000.

The contract behind T1 is `docs/remote/principal-contract.md` (G-PRINCIPAL). The section numbers below refer to it.

## T1: the unit

```bash
sudo install -m 0644 scripts/server/ikenga-server-t1.service /etc/systemd/system/
# First admin, before the first start (or use IKENGA_BOOTSTRAP_ADMIN, §7.4):
sudo /opt/ikenga/bin/ikenga-server accounts --data-dir /opt/ikenga/data create ada --admin
sudo systemctl daemon-reload && sudo systemctl enable --now ikenga-server-t1
# Check the host before starting, or after a failed start:
sudo /opt/ikenga/bin/ikenga-server probe --executor-tier t1 --data-dir /opt/ikenga/data
```

The unit differs from the T0 one in four ways (§8 "Deploy consequence"):

- **Root, with a cut capability set.** `CapabilityBoundingSet=CAP_SETUID CAP_SETGID CAP_CHOWN CAP_KILL CAP_DAC_OVERRIDE CAP_FOWNER`, and `NoNewPrivileges=true`. The broker's boot probe refuses to start without root and the first four. It also does a real test drop to the reserved probe uid. Only auth, the reverse proxy and the per-principal child launch run as root. Every RPC, terminal and engine runs inside a child that has already dropped to that person's uid.
- **Writable paths.** `ProtectSystem=strict` with `ReadWritePaths=/opt/ikenga/data /etc`.
  - **This deviates from G-PRINCIPAL §8's "Deploy consequence"**, which names only `/etc/{passwd,group,shadow,gshadow}`. The deviation is pending a remote-access Round that records it as a §14 amendment (any change to §8 needs one); until then this README and the unit's comments are where it is written down. Listing those four files doesn't work. Both provisioning backends write a sibling file and rename it over the original, and `lckpwdf(3)` creates `/etc/.pwd.lock`. A read-only `/etc` allows neither the new sibling nor a rename onto a bind-mounted file.
  - To keep `/etc` read-only, pre-create users yourself, set `IKENGA_PROVISIONING=external`, map each account with `accounts create <name> --adopt-unix-user <user>`, and drop `/etc` from the line.
- **Adopted homes.** An adopted account (below) keeps its existing passwd home. Add that home to `ReadWritePaths`.
- **`KillMode=process`.** This is the detached chi-runner fix (§9.4, owed by WP-18b), explained in the next section.

### Detached chi-runners survive a restart

A persistent Chi run is a `chi-runner` that the principal's child spawns detached, in its own process group. A process group stays inside the unit's cgroup, so the default `KillMode=control-group` would SIGKILL every runner on `systemctl restart`. With `KillMode=process`, systemd signals only the broker:

- Each principal child notices within a second that its parent is gone, drains its PTYs and exits. Terminals don't survive a restart, by design (ADR-023 D5).
- The runners keep going and keep writing their status files.

Until an old child has exited, it still holds its principal's `<data>/.lock` (I-3: one opener per `ikenga.db`). That is up to about a second after the restart, plus however long its PTYs take to drain. If that principal's first request reaches the new broker inside that window, the broker's fresh child fails the lock and exits, and the request fails once with "the principal child exited before it was ready". The next request, a moment later, starts the child normally. Reload the page if a tab shows that error right after a restart.

The cost is that any other orphan a person left running (`nohup … &`) also survives a restart. To stop everything, run `systemctl kill --kill-who=all ikenga-server-t1`, or disable the account. Disabling kills every process of that uid (§7.3).

Wrapping each runner in `systemd-run --scope` is not an option under T1. The runner's parent is an unprivileged principal child, which cannot create scopes in the system manager.

## T0 → T1: `accounts adopt-t0`

```
ikenga-server accounts --data-dir <operator-root> adopt-t0 --from <old-data-dir> --home <old-home> <username>
```

Stop the T0 daemon first. `adopt-t0` refuses while the pid in `<old>/daemon.json` is alive.

For an adopted user (the VPS case below), `adopt-t0` first **kills every process of that uid** (§7.3), so nothing of it is still inside the old dir while root moves and chowns it. That includes SSH sessions of that user. So don't run it from a login of the user being adopted, even through `sudo`. It refuses if one of its own parent processes runs as that uid. Log in as root or as another admin, or detach it from your session:

```bash
sudo systemd-run --wait --pipe --collect ikenga-server accounts --data-dir /opt/ikenga/data adopt-t0 --from /opt/ikenga/data-t0 --home /home/ikenga ada
```

`adopt-t0` also refuses, before moving anything, an adopted tree that holds a hard-linked file (it may also be named outside the tree: break the link with `cp -p f f.new && mv f.new f`) or a mount point. It never follows a symlink in the old dir.

The T1 operator root must be a different directory from the old T0 data dir; the two can't overlap. Move the old one aside if you want T1 at the same path:

```bash
sudo systemctl disable --now ikenga-server
sudo mv /opt/ikenga/data /opt/ikenga/data-t0
```

### VPS install (T0 ran as the non-root `ikenga` user)

```bash
sudo ikenga-server accounts --data-dir /opt/ikenga/data create ada --admin --adopt-unix-user ikenga
sudo ikenga-server accounts --data-dir /opt/ikenga/data adopt-t0 --from /opt/ikenga/data-t0 --home ~ikenga ada
```

The account maps onto the existing uid and home (`adopted = 1`). The data dir moves into `principals/<id>/data/`, by rename when it is on the same filesystem and by copy + verify otherwise (including when the rename fails because the data dir is reached through a bind mount). Paths under the home stay valid because the home didn't move.

### Docker / root install

```bash
ikenga-server accounts --data-dir /opt/ikenga/t1 adopt-t0 --from /opt/ikenga/data --home /root --admin ada
```

Root can't be adopted (I-1), so `adopt-t0` creates `ada` as a fresh principal. It prompts for the password, or reads it with `--password-stdin`. It then copies:

- the data dir;
- the old home's `.ikenga`, `.local/share/app.ikenga`, `.agent-ops`, `.atelier`, `.claude`, `.claude.json`, `.codex` and `.gemini`.

It chowns everything to the new uid. `fs_roots.json` entries under the old home are rewritten onto the new one.

### Either way

- The old dir is kept as `<old>.t0-migrated-<ts>`, root-owned and read-only. It holds the T0 access store (`access.db`, `-wal`, `-shm`) and the old `daemon.json`, none of which is ever carried into the principal's data dir (G-ACCESS R-10).
  - Under a rename, those files are all the archive holds.
  - When the old dir is a mount point (a Docker volume), it can't be renamed. It stays in place, read-only. The archive-only files are copied out, verified, and then deleted from it.
- Every migrated file ends up owned by the principal with no group/other bits, and the command checks I-9 before it reports success.
- If it fails after the data has moved, it still seals the archive and writes a partial `MIGRATION-REPORT.json` (with an `error` field), and the error message names both. Running it again won't finish the job, because the principal is no longer fresh: finish by hand from the report. A failure before the data moves changes nothing, apart from an account the command created. Re-running the same command reuses that account, `--admin` included.
- `<old>.t0-migrated-<ts>/MIGRATION-REPORT.json` lists what moved and what was rewritten. It also lists every `ikenga.db` column that still holds a path under the old home or data dir. **Those paths are not rewritten.**
- Chi runs that were live under T0 come back as `Unverified`.
- T0 paired devices don't carry over. Re-pair them.
