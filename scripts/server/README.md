# Deploying `ikenga-server`

`ikenga-server` is the headless daemon: the same workspace as the desktop app, served to a browser, with no display needed. Run it on a Linux machine you control. This guide covers installing it from a release, choosing a mode, and starting it under systemd.

| Mode | Flag | Runs as | Who signs in | Unit |
|---|---|---|---|---|
| **Multi-user** (use this for a team, or for any instance more than one person can reach) | `--executor-tier t1` | root (the broker); each person's work runs as their own Unix user | local accounts, username and password at `/auth/login` | `ikenga-server-t1.service` |
| **Single-user** | `--executor-tier t0` (the default) | the `ikenga` service user | anyone holding the bearer token (`IKENGA_AUTH_TOKEN`) | `ikenga-server.service` |

Install **one** of the two units. Both bind port 4000.

The contract behind multi-user mode is `docs/remote/principal-contract.md`, and the section numbers below refer to it.

## What you need

- A Linux host with systemd and root access. The release is built for **x86_64 and arm64**. arm64 is newer and has had less testing than x86_64.
- glibc. Debian 12 or Ubuntu 22.04 and newer are what the builds are tested on; older systems are unverified. There is no musl or Alpine build.
- `ca-certificates`, `curl`, `git`, `tmux` and `procps`. Install [Bun](https://bun.sh) as well, for agent CLIs and helpers that ship as JavaScript.
- Multi-user mode needs real root, not just a few capabilities. It will refuse to start without it. Inside a container, see [Containers](#containers).

A server install serves the terminal, chat, files and settings. It does **not** ship with mini-apps: none are staged, and the daemon never installs any itself. Browser-run mini-apps are not available on a server yet.

## Install from a release

Server tarballs are attached to each [GitHub release](https://github.com/ikenga-hq/ikenga/releases) next to the desktop installers. Replace `X.Y.Z` with the release you want and `amd64` with `arm64` on an ARM host.

```bash
V=X.Y.Z; A=amd64
BASE=https://github.com/ikenga-hq/ikenga/releases/download/v$V
curl -fsSLO $BASE/ikenga-server_${V}_linux_$A.tar.gz
curl -fsSLO $BASE/SHA256SUMS.txt
sha256sum -c --ignore-missing SHA256SUMS.txt     # must say OK for the tarball

sudo install -d -m 0755 /opt/ikenga
sudo tar -xzf ikenga-server_${V}_linux_$A.tar.gz -C /opt/ikenga
/opt/ikenga/bin/ikenga-server --version          # prints X.Y.Z
```

The tarball unpacks to `/opt/ikenga/{bin,dist}` plus both unit files. A matching checksum shows the download is intact. To check it was built by this repository's release workflow, verify the build attestation, or the signature:

```bash
gh attestation verify ikenga-server_${V}_linux_$A.tar.gz -R ikenga-hq/ikenga

curl -fsSLO $BASE/ikenga-server_${V}_linux_$A.tar.gz.sigstore.json
cosign verify-blob --bundle ikenga-server_${V}_linux_$A.tar.gz.sigstore.json \
  --certificate-identity-regexp '^https://github.com/ikenga-hq/ikenga/.github/workflows/release.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ikenga-server_${V}_linux_$A.tar.gz
```

To build from source instead, use `deploy.sh` in this directory. It is the developer path, needs the sibling `contract` and `tokens` repositories, and stages `out/{bin,dist}` for you to copy to `/opt/ikenga`. See its header. From a source checkout, take the unit files from this directory.

## Environment file

The units read `/opt/ikenga/.env`. It holds secrets, so it is root-only and you create it before the first start. Edit it by hand and back it up before you rewrite lines in it.

```bash
sudo install -d -m 0700 /opt/ikenga/data
sudo touch /opt/ikenga/.env && sudo chmod 600 /opt/ikenga/.env
```

| Variable | Mode | What it does |
|---|---|---|
| `IKENGA_HOST` | both | Bind address. Unset, it is `127.0.0.1`, which is unreachable from other machines: the safe default. Set it to the host's Tailscale address, or leave it unset and put a reverse proxy on loopback in front. Do not bind a public address directly. |
| `IKENGA_PUBLIC_URL` | both | The https address people reach the server on. It is the base of pairing and invite links. |
| `IKENGA_TRUSTED_PROXIES` | both | Comma-separated list of trusted reverse proxy IP addresses and CIDR networks (e.g. `127.0.0.1,::1,10.0.0.0/8`). When set and the direct TCP peer matches, the server resolves client IP addresses from `Forwarded` or `X-Forwarded-For` headers (right-most untrusted hop). Unset by default (forwarded headers ignored). Behind Caddy on loopback (`PERIMETER=public-https`), set this to `127.0.0.1`. |
| `IKENGA_INSECURE_COOKIE` | multi-user | `true` only for plain HTTP on a private network such as a tailnet. Leave it unset behind HTTPS so the session cookie stays `Secure`. |
| `IKENGA_UID_RANGE` | multi-user | Unix uid range for accounts, default `20000-29999`. It must match the range the data directory was first set up with. |
| `IKENGA_AUTH_TOKEN` | single-user | The bearer token. Without it the daemon mints a random one on every start, so every client breaks after a restart. Ignored in multi-user mode. |
| `ANTHROPIC_API_KEY`, `GEMINI_API_KEY` | single-user | Inherited by the agent CLIs the daemon starts. Optional. |

For a single-user host, generate the token without printing it:

```bash
sudo sh -c 'umask 077; printf "IKENGA_AUTH_TOKEN=%s\n" "$(openssl rand -hex 32)" >> /opt/ikenga/.env'
```

Read it back, when you need to sign a client in, with `sudo grep '^IKENGA_AUTH_TOKEN=' /opt/ikenga/.env`.

Keeping long-lived credentials in this file lets the daemon start unattended. The trade is that **anyone who gets onto the box gets those credentials**. Do not put anything in it you would not leave on the host.

The daemon speaks plain HTTP and has no TLS of its own. Reach it over a private network, or put an HTTPS proxy in front. When running behind a reverse proxy (such as Caddy), set `IKENGA_TRUSTED_PROXIES=127.0.0.1` so client addresses are resolved from forwarded headers rather than collapsing onto loopback. Without this setting, device-pairing throttling, login backoff and access audit rows are shared across all clients behind the proxy.

## Multi-user: the unit, the first admin, the first start

```bash
sudo install -m 0644 /opt/ikenga/ikenga-server-t1.service /etc/systemd/system/
# Check the host first. Read-only; exits 0 when multi-user mode can run, 1 when it can't:
sudo /opt/ikenga/bin/ikenga-server probe --executor-tier t1 --data-dir /opt/ikenga/data
# The first admin, before the first start (prompts for the password; or use IKENGA_BOOTSTRAP_ADMIN, §7.4):
sudo /opt/ikenga/bin/ikenga-server accounts --data-dir /opt/ikenga/data create ada --admin
sudo systemctl daemon-reload && sudo systemctl enable --now ikenga-server-t1
curl -fsS http://127.0.0.1:4000/api/health
```

Then sign in as that admin in a browser, at the address the server is reachable on.

If the service fails to start, run the `probe` line again. The server never falls back to a weaker mode: a failed probe means it stops.

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

## Single-user: the unit

For one person, or a private box where everyone sharing the bearer token is trusted. The unit runs as the `ikenga` user and writes its state to `/opt/ikenga/data`:

```bash
sudo useradd --system --create-home --home-dir /home/ikenga --shell /bin/bash ikenga
sudo chown ikenga:ikenga /opt/ikenga/data
# /opt/ikenga/.env holds IKENGA_AUTH_TOKEN (see "Environment file")
sudo install -m 0644 /opt/ikenga/ikenga-server.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now ikenga-server
curl -fsS http://127.0.0.1:4000/api/health
```

The unit makes the whole filesystem read-only except `/opt/ikenga/data`. Add the service user's home to its `ReadWritePaths` (see the comment in the unit), or a terminal cannot write to its own home.

## Containers

The `Dockerfile` and `docker-compose.yml` in this directory are an older single-user image built from `deploy.sh` output. They are not a supported way to run a team instance. Do not use them for one. Multi-user mode inside a container needs root, and the capability set the unit above grants: `--cap-drop ALL`, then add back `CHOWN`, `SETUID`, `SETGID`, `KILL`, `DAC_OVERRIDE` and `FOWNER`. A container also has no `KillMode=process`, so a restart ends the persistent agent runs that the systemd unit would have kept alive.

## State and backups

Everything lives under `/opt/ikenga/data`. In multi-user mode that is the operator root: `operator/` (root-only: `accounts.db`, `sessions.db`, `secrets-kek`) and `principals/<id>/{home,data}` for each person.

- `operator/secrets-kek` is 32 random bytes. **If you lose it you lose every member's stored secrets**, and the server will not mint a new one while any secret store exists. Keep a copy somewhere other than your data backups.
- There is no `ikenga-server backup` command yet. The databases are SQLite: copy them with `sqlite3 <db> "VACUUM INTO '<new file>'"` or `.backup`, never with `cp` on a running server. Back up the home trees with your usual tooling.
- A volume snapshot of this directory captures the key file together with the data it protects, so treat such snapshots as secret.

## Verifying a running daemon

`verify-live.ts` drives a daemon's real HTTP and WebSocket surface, with real terminals and reconnects. It is a developer tool for a source checkout and runs in single-user mode with the bearer token. For an installed instance, the checks above (`probe`, `/api/health`, signing in) are the post-install test.

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
