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
| `IKENGA_TRUSTED_PROXIES` | both | Comma-separated trusted reverse-proxy addresses and CIDR networks (e.g. `127.0.0.1,::1,10.0.0.0/8`). Unset, every forwarded header is ignored and the server uses the TCP peer's address, as before. Set, and only for a request whose TCP peer is in the list, the client address is taken from the one header named by `IKENGA_TRUSTED_PROXY_HEADER`: the right-most hop that is not itself in the list. A hop that is not an IP address, or a malformed header, falls back to the TCP peer. Behind Caddy on loopback (`PERIMETER=public-https`), set `127.0.0.1`. If the daemon binds a dual-stack address (`[::]`), an IPv4 proxy still matches its IPv4 entry. Invalid entries are logged once at start and skipped. |
| `IKENGA_TRUSTED_PROXY_HEADER` | both | The one forwarding header your proxy overwrites: `x-forwarded-for` (the default; right for Caddy and nginx) or `forwarded` (RFC 7239). Only that header is read. The other is ignored, because Caddy and nginx pass a client's own `Forwarded` header through untouched. Set `forwarded` only if your proxy overwrites that header. Any other value logs a warning and uses `x-forwarded-for`. Ignored when `IKENGA_TRUSTED_PROXIES` is unset. |
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

The daemon speaks plain HTTP and has no TLS of its own. Reach it over a private network, or put an HTTPS proxy in front. When running behind a reverse proxy (such as Caddy), set `IKENGA_TRUSTED_PROXIES=127.0.0.1` so client addresses are resolved from forwarded headers rather than collapsing onto loopback. Without this setting, device-pairing throttling, login backoff and access audit rows are shared across all clients behind the proxy. The setting only changes which address is throttled and recorded. Whether the device cookie is marked `Secure` is still decided by the TCP peer, so behind a plain-HTTP proxy pairing can still fail with `cookie_rejected` unless `IKENGA_INSECURE_COOKIE` is set.

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

The unit differs from the T0 one in five ways (§8 "Deploy consequence", plus the WP-P10 `/proc` rule):

- **Root, with a cut capability set.** `CapabilityBoundingSet=CAP_SETUID CAP_SETGID CAP_CHOWN CAP_KILL CAP_DAC_OVERRIDE CAP_FOWNER`, and `NoNewPrivileges=true`. The broker's boot probe refuses to start without root and the first four. It also does a real test drop to the reserved probe uid. Only auth, the reverse proxy and the per-principal child launch run as root. Every RPC, terminal and engine runs inside a child that has already dropped to that person's uid.
- **Writable paths.** `ProtectSystem=strict` with `ReadWritePaths=/opt/ikenga/data /etc`.
  - **This deviates from G-PRINCIPAL §8's "Deploy consequence"**, which names only `/etc/{passwd,group,shadow,gshadow}`. The deviation is pending a remote-access Round that records it as a §14 amendment (any change to §8 needs one); until then this README and the unit's comments are where it is written down. Listing those four files doesn't work. Both provisioning backends write a sibling file and rename it over the original, and `lckpwdf(3)` creates `/etc/.pwd.lock`. A read-only `/etc` allows neither the new sibling nor a rename onto a bind-mounted file.
  - To keep `/etc` read-only, pre-create users yourself, set `IKENGA_PROVISIONING=external`, map each account with `accounts create <name> --adopt-unix-user <user>`, and drop `/etc` from the line.
- **Adopted homes.** An adopted account (below) keeps its existing passwd home. Add that home to `ReadWritePaths`.
- **`KillMode=process`.** This is the detached chi-runner fix (§9.4, owed by WP-18b), explained in the next section.
- **`ProtectProc=invisible`.** `/proc` is mounted `hidepid=invisible` inside the unit, so a person's processes can't see anyone else's there. This is defense in depth, not a guarantee: someone signed in over SSH uses the host `/proc`, where it doesn't apply. Nothing depends on it. Every Chi engine (`claude-code`, `codex`, `antigravity-cli`, `opencode`, `pi`) reads its prompt from stdin, never from a command-line argument that `/proc/<pid>/cmdline` would show to other users (I-7). The same holds for the chat socket (`/ws/chat/:id`, default engine `antigravity-cli`), which sends each turn to `agy` on stdin as well. So all of them run under T1 with or without this line, including in the Docker deploy.

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

## Updates

`provision.sh` installs a stable copy of itself at `/usr/local/sbin/ikenga-provision` and four systemd units. You can also run `sudo ikenga-provision install-update-units` on its own.

| Unit | What it does |
|------|--------------|
| `ikenga-update-check.timer` → `ikenga-update-check.service` | Once a day, reads the `stable` release manifest and writes `/var/lib/ikenga-update/available.json`. Installs nothing. |
| `ikenga-update.path` → `ikenga-update.service` | When an admin asks for the update in the app, runs `ikenga-provision apply-request`: the same steps as `upgrade` (checksum, keep `.prev`, restart, health check that the new version answers, automatic rollback). Writes `/var/lib/ikenga-update/status.json`. |

**Who may update.** In multi-user mode, an enabled admin who is signed in with a password, or on a device at the Full tier. In single-user mode, only the bearer-token holder; a paired device cannot. Everyone else sees nothing. The app shows how many open terminals a restart will end and asks the admin to confirm.

**Notify only.** Nothing installs itself. An update happens only when an admin confirms it, or when you run `upgrade` over SSH.

**What root trusts.** The server writes nothing but a small request file: `/opt/ikenga/data/update-request.json` in single-user mode, `/opt/ikenga/data/operator/update-request.json` in multi-user mode. Root claims it, checks its owner, size and age (15 minutes), and applies it only when the version equals the one root's own check advertised. A request cannot name a URL, a path, a channel or a downgrade. After a rolled-back or failed attempt at a version, the same version is refused for an hour, so a broken release cannot turn into a restart loop.

**Files.** `available.json` and `status.json` are world-readable and hold versions, times and the last run's progress lines. `last-run.log` is root-only. None of them holds a secret, and the update path never reads `.env` except for the `IKENGA_HOST` line used for the health check.

**Exit codes of `upgrade`.** 0: upgraded, or already on that version. 3: the new version failed its health check and the previous one is back. 4: the rollback failed too; look at `journalctl -u ikenga-server*`.

**Turning it off.** `sudo systemctl disable --now ikenga-update.path` stops in-app updates. `sudo systemctl disable --now ikenga-update-check.timer` stops the daily check, and the app then shows no update at all.

## Shared projects and scoped secrets

Two things every account on a multi-user host needs, and neither belongs in the `IKENGA_SECRET_*` environment (which every account inherits) or in each account's home. `provision.sh` converges both from the profile. Accounts are still created by `ikenga-server accounts create`; the provisioner only names them, so run `sudo ikenga-provision sync-accounts` (or re-run the full provision) after creating or removing one. `--dry-run` prints the plan (`would: ...`) and changes nothing.

```bash
ACCOUNTS=(ada grace rex ruby)       # login names; the Unix user is ik-<name>. Empty = every ik-* user in UID_RANGE
AGENT_ACCOUNTS=(rex ruby)           # who a secret scoped `agents` goes to
PROJECTS=("app=https://github.com/org/app.git#main" "site=git@github.com:org/site.git")
SECRETS_FILE=/root/provision/secrets.scoped
```

An account named in the profile that has no Unix user yet is reported as pending and skipped.

### Shared projects: a read-only mirror, one clone per account

One download per project, everyone on their own branch, and nobody able to affect anyone else:

- **The mirror.** One bare repo per project in `PROJECTS_DIR` (default `/srv/ikenga/projects/<name>.git`). It is `root:root`, directories `0755` and files `0644` (or `0750`/`0640` plus read-only ACLs, below), with no group, no ACL and no file a member can write. Only the provisioner updates it. It is never gc'd or pruned (the clones depend on its objects).
- **Each account's own clone.** `~/projects/<project>`, created once as that user with `git clone --no-local --reference <mirror> <mirror>` and checked out on `<account>/main` (`PROJECTS_BRANCH_PREFIX` prepends to it) from the mirror's default branch. `origin` is the mirror path and `objects/info/alternates` points at the mirror, so the project is stored once. Its hooks, config, refs and index belong to the account. Account A can neither run code as account B through a git hook or config key, nor rewrite B's branches, nor read B's index. An existing clone is never touched; to refresh it the account runs `git fetch origin` itself, which only reads the mirror.

| Key | Default | |
|-----|---------|--|
| `PROJECTS` | none | `name=git-url[#branch]`. No `#branch` = the remote's default branch. A URL with credentials in it is refused. |
| `PROJECTS_DIR` | `/srv/ikenga/projects` | |
| `PROJECTS_READ` | `members` | `members`: the managed accounts get a **read-only** ACL entry on each mirror and nobody else can read it (right for private repos). `world`: any user on the box can read the mirrors. |
| `PROJECTS_MEMBERS` | all managed accounts | Who gets a clone and (with `PROJECTS_READ=members`) read access. Narrowing it removes the ACL entry; the account's clone is left where it is. |
| `PROJECTS_TOKEN_SECRET` | none | Name of a secret in `SECRETS_FILE` (scope it `root`): an https deploy token for private repos. |

**How the provisioner updates the mirror without trusting anything.** Root's `git fetch` runs with an empty environment (`env -i`), `HOME` in a root-only scratch directory, system and global config disabled, its working directory in that scratch directory, `core.hooksPath=/dev/null`, `core.fsmonitor=false`, no credential helper, no redirects, gc and maintenance off, and `GIT_ALLOW_PROTOCOL` narrowed to the one transport the profile URL uses. It fetches the profile's URL explicitly, so no `remote.*` or `url.*.insteadOf` in the repo is ever consulted, and it rewrites the mirror's `config` to a fixed canonical one on every run, empties `hooks/`, and removes symlinks and `alternates` files. The deploy token reaches `git` through `GIT_ASKPASS` (a script reading a `0600` file in the scratch directory) for http(s) URLs only: never argv, the URL or a repo config. Errors are captured in a variable, not written to a file. Root writes nothing into an account's home or any other directory an account can write. `sync-accounts` converges an older group-writable layout back to this one.

**Why `PROJECTS_READ=members` uses ACLs, not a group.** The daemon starts every account's sessions with **no supplementary groups** (`src-tauri/src/executor/t1.rs`, `verify_dropped()` refuses a spawn that has any), so group membership would help an SSH login but not a terminal or Chi run. A named-user ACL entry is matched on the uid and survives the drop. The entries are read-only (`r-x` on directories, `r--` on files); nothing grants write. The filesystem must support ACLs (ext4 and xfs do by default); if `setfacl` fails the run warns and exits non-zero.

**`safe.directory`.** Git refuses to fetch or clone from a repository owned by another uid. `/etc/gitconfig` (root-only) therefore carries an `# ikenga:` block that lists exactly the mirror paths. It is regenerated from `PROJECTS` on every run, so a project removed from the profile loses its entry (its mirror and the accounts' clones stay on disk). Each change to `/etc/gitconfig` or `/etc/bash.bashrc` leaves a `.bak-<time>` beside it; the last five are kept.

Mirrors refresh only when `sync-accounts` runs (put it on a timer if you want them regular); accounts have no upstream credential. A run with nothing new reports `no changes`.

### Scoped secrets

`SECRETS_FILE` must be a regular file, owned by root, mode `0600`, in a directory only root can write. A looser one is refused, and nothing is read. One secret per line, the scope in front, so the value is everything after the first `=` and is never interpreted:

```
# comments and blank lines are skipped
[everyone]     SHARED_NOTE=anything, including = # and spaces
[agents]       ANTHROPIC_API_KEY=...          # AGENT_ACCOUNTS
[rex]          MM_BOT_TOKEN=...               # one account (login name)
[ada,grace]    SOME_KEY=...                   # several
[root]         GIT_DEPLOY_TOKEN=...           # provisioner only, delivered to nobody
[backup]       ORDERS_DB_CONNECTION_STRING=...   # the backup user only (see Database backups)
```

There is no default scope: a line without one is an error, as is a scope naming an account that is not managed (a typo), an empty or multi-line value, a name that would change how a shell or git behaves (`PATH`, `LD_*`, `BASH_*`, `IKENGA_*`, `GIT_ASKPASS`, ...), or the same name reaching one account from two lines. Values are single-line and kept byte for byte.

Each account gets `/etc/ikenga/secrets/ik-<name>.env`, owned `root:<its private group>`, mode `0640`: it can read the file, not change it, and no other account can open it. The summary lists secret **names** per account (`+` added, `~` value changed, `-` removed), never values. Removing a line or narrowing its scope removes the secret from that account on the next run; an account dropped from the profile loses its file. Every replaced or removed file is first copied to `/etc/ikenga/secrets-backup/` (root-only, `0700`; the last five per account). The backups are deliberately not in the account-readable directory, which would undo a narrowing.

`SECRETS_FROM` (secrets copied into `/opt/ikenga/.env` as `IKENGA_SECRET_*`) still works and is still box-wide. Use `SECRETS_FILE` for anything that should not reach every account.

### Where the secrets reach, and where they do not

The daemon has no way for the root side to inject a per-account environment: a principal's child gets only a fixed floor plus the box-wide `IKENGA_SECRET_*` (`src-tauri/src/server/broker/children.rs`, `host_env()`; `src-tauri/src/executor/t1.rs`, `environment()`), and `IKENGA_SECRET_*` is stripped from PTYs and Chi runs on purpose (`pty/mod.rs`, `is_host_only_env`). Its per-account secret store is sealed and written only through the account's own RPC. So delivery is by the shell:

- `/etc/profile.d/ikenga-secrets.sh` exports the account's file (read line by line, `export "NAME=value"`, never evaluated), and a managed block at the top of `/etc/bash.bashrc` loads it for interactive bash.
- **Reached:** SSH logins, and any terminal whose shell is a login shell or interactive bash, and everything started from it (an agent CLI you launch at that prompt inherits them).
- **Not reached:** a process the daemon execs directly, with no shell in front: an engine CLI started as the terminal's own command, a Chi run, a pkg sidecar, and a non-login `sh` (the default account shell is `/bin/sh`, dash).

The fix for the second group belongs in the daemon: `T1Launcher::host_env()` reading the root-owned `/etc/ikenga/secrets/<unix_name>.env` and adding its entries to the child's environment, under names the PTY denylist does not match, would carry them into every PTY and Chi run. The file format is a plain `NAME=value` list so that change needs nothing new from the provisioner. Until then, treat agent-account secrets delivered this way as available to shell-launched work only.

## Database backups

Postgres backups run **on this box**, as system jobs, by a dedicated user that holds only the database connection strings and the GCS credentials (decision D-B10; they moved here from rex-vps). Rex's alerts only *read* a status file the jobs leave behind. `sudo ikenga-provision backups` (or the full provision run) converges all of it from the profile; `--dry-run` prints the plan by secret and file **name**, never a value.

```bash
BACKUPS_ENABLED=1
BACKUP_CONFIG=/root/provision/backup-config.json      # which databases, see below
BACKUP_GCS_KEY_SECRET=GCS_BACKUP_KEY_B64              # NAME of a SECRETS_FILE entry
SECRETS_FILE=/root/provision/secrets.scoped           # holds the [backup] connection strings and the key
# optional:
BACKUP_USER=ikenga-backup                             # default
BACKUP_SCHEDULES=("daily=*-*-* 03:30:00 UTC")         # name=OnCalendar; overrides or adds a schedule
BACKUP_PG_MAJOR=17                                    # postgresql-client-<n> from PGDG
BACKUP_TIMEOUT_SEC=10800                              # a run is killed after this (keep it under the shortest interval)
BACKUP_GCLOUD_KEY_FPRS=()                             # extra accepted Google apt signing-key fingerprints
```

| Key | Default | |
|-----|---------|--|
| `BACKUPS_ENABLED` | `0` | `1` installs and starts the timers. Back to `0` removes them (see "Converge" below). |
| `BACKUP_USER` | `ikenga-backup` | A plain system user: no login shell, no supplementary groups, no home except its `0700` private state directory. Not an Ikenga principal: `root`, the admin, the T0 `ikenga` user, any `ik-*` name and any uid inside `UID_RANGE` are refused. |
| `BACKUP_CONFIG` | none (required) | Path to a **JSON file** (not an inline list: it is data, it is reviewed on its own, and the same file is installed for the job). Shape = rex-vps's `backup-config.json`; an example with the nine Royalti databases is `scripts/server/backup/backup-config.example.json`. Only `databases[]` is read. |
| `BACKUP_GCS_KEY_SECRET` | none (required) | The **name** of the `SECRETS_FILE` entry holding the service-account key. |
| `BACKUP_SCHEDULES` | see below | `name=OnCalendar` entries that override or add schedules. |

`BACKUP_CONFIG` per database: `name` (also the object folder: letters, digits, `. _ -`), `connection_secret` (the **name** of a `[backup]` secret), `schedule` (a schedule name), `gcs_bucket`, `enabled` (default `true`). It holds names only, never a value, and is installed (`0640`, `root:<backup group>`) as `/etc/ikenga-backup/backup-config.json`. A schedule name with no calendar, a duplicate `name`, a bad bucket, or a secret name that is reserved is refused with the entry number.

**Schedules.** One systemd timer per schedule that some enabled database uses. Defaults match rex-vps's crontab, in **UTC**:

| Schedule | OnCalendar | rex-vps cron |
|----------|------------|--------------|
| `4hourly` | `*-*-* 00/4:00:00 UTC` | `0 */4 * * *` |
| `daily` | `*-*-* 02:00:00 UTC` | `0 2 * * *` |
| `weekly` | `Sun *-*-* 03:00:00 UTC` | `0 3 * * 0` |
| `6hourly`, `12hourly`, `monthly` | `00/6`, `00/12`, `*-*-01 04:00` | (defined in the old config, unused) |

Every timer has `Persistent=true` (a run missed while the box was down fires at boot) and `RandomizedDelaySec=120`. `BACKUP_SCHEDULES` expressions are checked with `systemd-analyze calendar` and may contain only what a calendar expression needs. The `schedules` cron table in an old config file is ignored.

### Secrets

The connection strings are `SECRETS_FILE` lines with the new scope **`backup`**:

```
[backup]  ROYALTIO_PROD_DB_CONNECTION_STRING=postgres://user:p%40ss@host:5432/db?sslmode=require
[root]    GCS_BACKUP_KEY_B64=<base64 of the service-account key JSON, on one line>
```

- `backup` is the backup user's alone. It **cannot be combined** with another scope, `everyone` and `agents` never include it, and the run refuses if a secret of the same name is also delivered to an account, or if a database's connection secret is in the file under any other scope. `backup` is now a reserved name (no account may be called that).
- **The key** is a multi-line JSON file and the secrets format is one line per value, so it is stored **base64-encoded**: `base64 -w0 key.json`. Scope it `[root]` (nobody gets it but the provisioner) or `[backup]`. It is decoded and checked (`type: service_account`, `private_key`, `client_email`) without printing anything; scoping it any other way is refused.
- **Connection strings** are `postgres://` or `postgresql://` URLs with the user and password percent-encoded. Supported query parameters: `sslmode`, `sslrootcert`, `connect_timeout`, `channel_binding`, `application_name`; anything else makes that database fail with `bad-connection-string` (a name, never the URL).
- Where they land: the strings in `/etc/ikenga-backup/connections.env` (`root:<backup group>`, `0640`, only the strings some enabled database uses, parsed line by line, never `source`d); the key in `/var/lib/ikenga-backup/private/gcs-key.json` (`ikenga-backup`, `0600`). Nowhere else: not argv, not a unit file, not the journal, not `status.json`. Backup-scoped secrets that no database refers to are named in the output and not written. A replaced env file is first copied to `/etc/ikenga/secrets-backup/` (root-only, last five).
- **How the job keeps them out of argv:** `pg_dump` is not given the URL. `run-backup.sh` parses it and passes `PGHOST`, `PGUSER`, `PGPASSWORD`, `PGDATABASE`, ... in the `pg_dump` process's environment (only the owner and root can read `/proc/<pid>/environ`; anyone can read `/proc/<pid>/cmdline`). `gcloud` is only ever given the key *path*.

### What is installed

| Path | Owner / mode | |
|------|--------------|--|
| `/usr/local/lib/ikenga-backup/run-backup.sh`, `verify-backup.sh` | `root:root` `0755` | Copied from `scripts/server/backup/` next to `provision.sh`. A copy of `provision.sh` run from elsewhere (the stable `/usr/local/sbin/ikenga-provision`) keeps the installed scripts. |
| `/etc/ikenga-backup/` | `root:<group>` `0750` | `backup-config.json` (names), `connections.env` (secrets). Ikenga accounts cannot even list it. |
| `/var/lib/ikenga-backup/` | `ikenga-backup` `0755` | **`status.json`** (`0644`, readable by everyone, no secrets). The directory is world-traversable so `status.json` can be read; nothing else in it is. |
| `/var/lib/ikenga-backup/private/` | `ikenga-backup` `0700` | The user's home: `gcs-key.json`, `gcloud/` (`CLOUDSDK_CONFIG`), `work/` (scratch), `errors/last-error-<db>.log` (raw tool output, `0600`, connection strings scrubbed; for the operator, not for the journal). |
| `ikenga-backup@.service`, `ikenga-backup-<schedule>.timer` | `root` | One oneshot service template (`%i` = the schedule) and one timer per schedule. |

**The service** runs as `ikenga-backup` with `NoNewPrivileges`, `ProtectSystem=strict` and `ReadWritePaths` = only `/var/lib/ikenga-backup`, `PrivateTmp`, `PrivateDevices`, `ProtectHome`, `ProtectProc=invisible`, kernel/clock/hostname/cgroup protection, `RestrictNamespaces`, `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK`, an empty capability set, `SystemCallFilter=@system-service`, and `InaccessiblePaths` for `/etc/ikenga` (account secrets), `/opt/ikenga` (daemon vault key) and `/srv/ikenga` (project mirrors). It is `Nice=10`, idle I/O, and killed after `BACKUP_TIMEOUT_SEC`. `MemoryDenyWriteExecute` is not set (gcloud is Python).

**Tools.** `pg_dump` comes from **`postgresql-client-17` in the PGDG apt repository** (`apt.postgresql.org`, `<codename>-pgdg`), because the production servers are PostgreSQL 17 and a `pg_dump` older than its server refuses to dump it; Ubuntu 24.04's own archive stops at 16. The repository's signing key is downloaded over https and **must have exactly the fingerprint `B97B 0AFC AA1A 47F0 44F2 44A0 7FCC 7D46 ACCC 4CF8`** (every key in the file must be a pinned one) before it becomes `/usr/share/keyrings/ikenga-pgdg.gpg`, which `signed-by` ties to this one repository; a mismatch stops the run with nothing installed. The job picks the highest installed `/usr/lib/postgresql/<n>/bin/pg_dump` and refuses anything below `BACKUP_PG_MAJOR`.

Uploads use **`gcloud storage cp`** (package `google-cloud-cli`, Google's apt repo `packages.cloud.google.com`, key pinned the same way to `35BA A0B3 3E9E B396 F59C A838 C0BA 5CE6 DC63 15A3`, "Artifact Registry Repository Signer", observed 2026-10-08), not `gsutil` (what rex-vps used): Google has put `gsutil` in maintenance and `gcloud storage` is the supported, faster replacement in the same package, with one authentication model. If Google rotates the key, provisioning stops naming the new fingerprint; confirm it against Google's install docs and add it to `BACKUP_GCLOUD_KEY_FPRS`. A `gcloud` already on the host is used as it is and nothing is installed. **Authentication** is the service-account key, activated on every run into `CLOUDSDK_CONFIG=/var/lib/ikenga-backup/private/gcloud`: the credentials never touch any other user's `~/.config/gcloud`, and rotating the secret needs nothing but a re-run of `backups`.

### What a run does

For each enabled database of the schedule, **in isolation** (one failing never stops the others; the run exits non-zero if any failed):

1. `pg_dump -w` (plain format, v17+) streamed straight into `gzip` (no uncompressed copy on disk; both halves' exit codes are checked);
2. `verify-backup.sh`: size, gzip integrity, the pg_dump header **and** the completion marker (so a dump cut short is rejected);
3. `gcloud storage cp` to `gs://<gcs_bucket>/<db>/<YYYY>/<MM>/<db>-<YYYYMMDD-HHMMSS>.sql.gz` (UTC; the database's own bucket);
4. the dump is deleted and `status.json` is updated.

The journal (`journalctl -u 'ikenga-backup@*'`) gets one line per database: `db=<name> status=ok bytes=N` or `db=<name> status=FAILED kind=<kind>`. **Names and kinds only.**

### `status.json` (what Rex reads)

```json
{
  "schema": 1, "enabled": true, "updated": "2026-10-08T02:00:41Z",
  "databases": {
    "royalti-prod-db": {
      "schedule": "4hourly",
      "last_attempt": "2026-10-08T00:00:12Z", "last_attempt_ok": true,
      "last_success": "2026-10-08T00:00:12Z",
      "last_error_kind": null, "last_error_at": null,
      "object": "gs://db-backups-archive/royalti-prod-db/2026/10/royalti-prod-db-20261008-000012.sql.gz",
      "bytes": 48211934, "duration_s": 31
    }
  },
  "schedules": { "4hourly": { "last_run": "...", "last_run_ok": true, "succeeded": 1, "failed": [] } }
}
```

`last_error_kind` is null after a successful attempt and otherwise one of `bad-config`, `no-secret`, `bad-connection-string`, `no-pg-dump`, `gcs-auth` (the key was rejected: nothing is dumped), `dump-failed`, `verify-failed`, `upload-failed`. `object` and `last_success` always describe the last **successful** upload; a failure never erases them. The file lists every enabled database from the moment of provisioning (a never-run one has nulls), and it is rewritten atomically after each database. `enabled` is `false` when backups are disabled.

**The alert contract for Rex's box-alerts schedule** (replaces `cron-alert.sh` and the fleet-audit log greps): read `/var/lib/ikenga-backup/status.json` and alert when `enabled` is false or the file is missing or older than a day; or for any database `last_attempt_ok == false`, or `last_success` is null or older than twice its schedule's interval (suggested: `4hourly` 9 h, `daily` 26 h, `weekly` 8 d, `monthly` 32 d). Also alert on `systemctl is-failed 'ikenga-backup@*'` and on a missing timer (`systemctl list-timers 'ikenga-backup-*'`). Nothing in the file is secret; the alert text can name the database and the kind.

### Converge

`sudo ikenga-provision backups` is idempotent: a rerun reports `no changes` and touches no unit. A changed `BACKUP_SCHEDULES` or config rewrites only the affected timers (and restarts those); a schedule no enabled database uses loses its timer and its status entries; a changed or removed `[backup]` secret updates `connections.env` (reported as `+NAME` / `~NAME` / `-NAME`); a rotated key is replaced. A connection secret missing from `SECRETS_FILE` is reported by database and name and makes the run exit non-zero, after everything else converged (that database reports `no-secret` until it is added).

**Disabling** (`BACKUPS_ENABLED=0`) removes the timers and the service unit **and the credentials** (the env file and the key; a root-only copy of the env file stays in `/etc/ikenga/secrets-backup/`). It keeps the user, the scripts, `status.json` (marked `enabled: false`) and the error logs. Enabling again restores timers and credentials from `SECRETS_FILE` and carries the history on.

### First run on the real box

The container test cannot run systemd's sandbox, so watch the first real run:

```bash
sudo ikenga-provision backups --profile <profile> --dry-run     # read the plan
sudo ikenga-provision backups --profile <profile>
systemd-analyze security ikenga-backup@daily.service            # exposure score
sudo systemctl start ikenga-backup@daily.service                # one real run
journalctl -u ikenga-backup@daily.service -n 30
cat /var/lib/ikenga-backup/status.json | jq .databases
sudo ls /var/lib/ikenga-backup/private/errors/                  # raw errors, if a database failed
```

If the sandbox blocks something (a `gcloud` that needs a path or a syscall outside the list), the journal says so; loosen that one directive in `backup_service_unit` rather than dropping the sandbox. **Not handled here:** restore drills (a periodic restore of the newest dump into a scratch database), bucket retention/lifecycle rules (objects are never deleted), a free-disk guard before a large dump, and the Rex side of the alert contract above. Also note that `gcloud auth activate-service-account` needs to reach `oauth2.googleapis.com` (it refreshes a token at activation), so a run with no outbound network reports `gcs-auth` for every database and dumps nothing.

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
