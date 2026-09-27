# G-PRINCIPAL — the principal and auth contract (T1, local accounts)

**Gate:** G-PRINCIPAL · **Owner:** remote-access WP-20 · **Status:** DRAFT — awaiting founder sign-off (§13) · **Written:** 2026-09-27

WP-21, WP-22 and shell-ux WP-73 (G-ACCESS) code against **this file**, not against a WP-20 merge. The gate was defined in remote-access Round 13 as freezing "the principal id, the session/token → uid mapping, and the per-principal data-dir layout", fixing `03` §l's singletons (`plans/remote-access/04-discussion.md:70`). This is a contract draft only: nothing here is implemented, and WP-20/21/22 must not start until §13 is signed.

**Grounded against:** `ikenga` `origin/main` at `df5482c` (WP-19 slices 1–3 #298/#301/#304, WP-18b part a #303). PR #305 (WP-18b part b) is **open**, not merged; where this file depends on it, it says so. Line numbers are anchors on `df5482c`. Plan paths (`plans/…`, `docs/adr/…`) are in the workspace meta-repo.

**Conventions.** *must* / *never* are normative. **Locked** = a founder decision this file records and does not reopen. **Proposed** = this file's recommendation, binding only once §13 is signed. Open forks are in §12 with a recommendation each.

---

## 0. Scope and decisions recorded

### 0.1 Locked (not reopened)

| Id | Decision | Source |
|---|---|---|
| **D1 / G-91** | Remote v1 is **T1**: a multi-user, mutually trusted team; per-user Unix uid, data dir, SQLite and credential store; spawns via setuid. T2 is Phase 5. | ADR-023 `:37`, `:55`; `04` Round 13 `:61` |
| **Floor** | T0 and T1 never require a privileged container, `NET_ADMIN`, a Docker socket, or host packages beyond the binary. T1's only privilege is root-in-container with `CAP_SETUID`/`CAP_SETGID`. | ADR-023 `:41`, `:76` |
| **D2 / G-92** | T1 hosts: VPS, Coolify, Fly.io, and Railway (G-73 spike, 9/9). Render is T0-only. | ADR-023 `:56`; `04` Round 14 `:36-46` |
| **D3 / G-93** | Local accounts in the daemon: argon2id hashes, server-side sessions (`axum-login` / `tower-sessions`), admin-created users, **no self-signup**. **One account = one principal = one Unix uid.** OIDC is WP-22 and maps an IdP identity onto an existing principal. No LDAP/AD. | ADR-023 `:57`; `04` Round 13 `:63` |
| **Credentials** | `SecretsStore` gains a principal scope; `IKENGA_SECRET_*` becomes the operator default a principal's own credential overrides (WP-21). | ADR-023 `:45` |
| **D4 / D5** | tmux is retired. Chi runs survive restarts through a detached chi-runner and a status file (shipped, #303). Terminals keep holder-through-crash + revive. No pty-host. | ADR-023 `:58-59`; `04` Round 12 `:118-119` |
| **DEC-R9-1** | The boot probe refuses a tier the host can't honour. Refuse, don't fall back. | `04` Round 9 `:306`; `server/mod.rs:334-349` |
| **Probe must be real** | WP-20's boot probe checks `CAP_SETUID`/`CAP_SETGID` **and** does a real test drop at every T1 boot rather than trusting the platform. | `04` Round 14 `:50` |

### 0.2 What main has today

- `Principal { id: String }` exists but nothing uses it. It is reserved on `SpawnSpec.principal` and "ignored by T0" (`src-tauri/src/executor/mod.rs:48-56`, `:84-85`, `executor/in_process.rs:19`).
- `probe()` passes T0 and refuses T1–T3 with `Refusal::NotImplemented`. It does no host check (`executor/tier.rs:133-151`).
- `/api/health` reports `executor.principal_isolation` from the **installed** executor (`server/health.rs:25-28`, `:45`). The route is **unauthenticated** (`server/mod.rs:323`).
- Auth is one bearer secret per daemon (`server/mod.rs:61`), minted if absent (`:359-362`). It is checked in constant time from a `Bearer` header or a `?token=` query (`:155-208`, `ct_eq` `:110`).
- Spawn routing: #303 put PTYs, the claude/codex/antigravity engines and the detached chi-runner on `executor::current()` (`pty/mod.rs:599`, `claude/session.rs:661`, `engines/codex_pty/engine.rs:230`, `engines/antigravity_acp/server.rs:314`, `commands/chi_runner.rs:97`). **#305 (open)** routes the rest: pkg MCP, sidecars, cron, `pkg_invoke`, installs, the playwright proxy, `action_exec`, `agent_detect` and chi's in-process engine. Until #305 merges, those sites still call `Command::new` directly.

### 0.3 Out of scope

Roles, device pairing, invites and the audit-log schema belong to G-ACCESS (WP-73..77). The SecretsStore envelope (server KEK vs user-derived key) is WP-21's (`02` §k). OIDC flows are WP-22's. T2/T3 are out. Seats are unaffected: they are per-project, not per-principal (`plans/shell-ux-rearchitecture/drafts/seats-schema.md:85`).

---

## 1. Principal id

**Proposed.** A principal id is an opaque **UUIDv7**, stored and sent as its lowercase hyphenated 36-char string. It is minted once when the account is created and **never reused**, even after the account is disabled. It is distinct from the username (mutable, human-facing) and from the uid (a host attribute).

WP-22 (OIDC `sub`), WP-21 (secret keys), and G-ACCESS (roles, devices, audit) all key on the id. That way, re-provisioning a host, renaming a user, or adding an IdP never changes the key. UUIDv7 needs no new crate: `uuid` is already a dependency at 1.25 (`src-tauri/Cargo.toml:69`, `Cargo.lock:8526-8527`), and WP-20 only adds the `v7` feature. v7 is time-ordered, which keeps the `accounts` PK index append-mostly. ULID would add a crate for the same property. The uid can't be the id, because a restored or migrated host may renumber it.

```rust
// executor/mod.rs — replaces today's `pub struct Principal { pub id: String }` (:53-56).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PrincipalId(uuid::Uuid);          // Display/FromStr/serde = lowercase hyphenated; parse rejects non-v7

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub id: PrincipalId,
    pub username: String,   // login name (display); mutable; NOT used for any path or key
    pub unix_name: String,  // passwd name; immutable once provisioned (§7.2)
    pub uid: u32,           // never 0; inside the operator's uid range unless adopted (§11.2)
    pub gid: u32,           // user-private group; == uid for allocated accounts
    pub home: PathBuf,      // absolute; the passwd home (§4)
    pub shell: PathBuf,     // passwd shell; /bin/sh when the operator sets none
}

// SpawnSpec keeps its shape: `pub principal: Option<Principal>` (executor/mod.rs:84-85).
```

`Principal` is **fully resolved before any spawn** (§9.1). The executor never reads the accounts DB, NSS or `/etc/passwd` on the spawn path.

---

## 2. Request → principal

### 2.1 One resolution point

Every authenticated request resolves to **exactly one** `PrincipalCtx` before it reaches a handler. Handlers never see a raw credential.

```rust
pub struct PrincipalCtx {
    pub principal: Principal,
    pub via: Credential,
}
pub enum Credential {
    Session { session_id: String },   // WP-20: browser cookie
    DeviceGrant { device_id: String },// reserved for WP-74; must resolve to a principal_id
    OperatorBearer,                    // T0 only (§2.4); never produced under T1
}
```

Every later credential kind (OIDC login, device grant, PAT) *adds a way to obtain* a `PrincipalCtx`. None of them bypasses it.

### 2.2 Browser: session cookie (Proposed shape; mechanism locked by D3)

- **Stack:** `axum-login` 0.16 over `tower-sessions` 0.13, with the `tower-sessions-sqlx-store` 0.14 `SqliteStore` in `operator/sessions.db` (§4, §6.3).
- **Login:** `POST /auth/login {username, password}` → `204` + `Set-Cookie`. axum-login cycles the session id on login.
- **Other routes:** `POST /auth/logout`. `GET /auth/me` → `{principal_id, username, is_admin}`. `POST /auth/password {current, new}`.
- **Unauthenticated routes:** only `/api/health`, `/auth/login` and the static SPA. Today that is `/api/health` and the SPA fallback (`server/mod.rs:322-326`).
- **Cookie:** `ikenga_session`, `HttpOnly`, `SameSite=Strict`, `Path=/`, `Secure` by default (§12.2 P-3). Expiry `OnInactivity(24h)`.
- **Staying valid:** `AuthUser::session_auth_hash` is derived from `(password_phc, session_epoch)`. A password change, a disable, or a forced logout bumps the epoch and invalidates every session of that principal on its next request (§6).
- **CSRF:** `SameSite=Strict` plus the existing `Origin` gate (`origin_permitted`, `server/mod.rs:131-153`), applied unchanged to every state-changing route and to every WebSocket handshake.

### 2.3 WebSockets and the `?token=` trick

`?token=` exists because a browser can't set headers on `new WebSocket` (`server/mod.rs:188-189`; FE `src/lib/transport/index.ts:202-203`, `chat-client.ts:6`). Browsers do send same-origin cookies on the WS handshake, so **under T1, `/ws/pty/:id`, `/ws/chat/:id` and `/ws/fs` authenticate by session cookie, and `?token=` is not accepted.**

A cookie also closes the gap `pkg_static.rs:25-39` records: iframe subresources arriving with no credential. That file names a cookie as "a daemon-wide auth change … deliberately out of scope". This contract is that change for T1. The single-file-bundle rule from G-95 (`04` Round 13 `:65`) stays in force for desktop parity.

### 2.4 The bearer token's fate (Proposed — §12 OD-6)

- **T0:** unchanged. `IKENGA_AUTH_TOKEN` / minted token, Bearer or `?token=` (`server/mod.rs:155-208`).
- **T1:** the operator bearer grants **no principal surface**: no shell, RPC or WS as anyone. Operator administration is local CLI only (§7); it needs root on the host, not a network credential. Remote non-browser clients (curl, iyke) authenticate as a principal through `POST /auth/login` and a cookie jar. Per-device credentials are WP-74's, which already "replace[s] the daemon's single shared bearer token" (`plans/shell-ux-rearchitecture/09-orchestration.md:1121`). The FE reauth dialog (`src/lib/transport/reauth-store.ts`) gains a username/password mode when `/api/health` reports `tier: "t1"`.

### 2.5 Ownership of live objects

Every PTY session, chat thread, chi run, fs watch and pkg MCP/sidecar child has an **owner principal**. Every attach / write / resize / kill (`server/pty_ws.rs:15-19`), prompt / cancel (`server/chat_ws.rs:242`, `:280`) and read checks it against `PrincipalCtx`.

Today there is no check: `resolve_id` resolves any id or label daemon-wide (`pty_ws.rs:61`, `pty/mod.rs:812`). `owner_agent_id` is a multi-agent lease, not a human owner (`pty/mod.rs:268`, `:937`). Id and label lookups are **namespaced by principal**, so another principal's id or label resolves to "gone", never "forbidden". An existence oracle leaks ids.

Under topology B (§3) this holds **by construction**: each principal's objects live only in that principal's process. Under A it is an explicit `owner: PrincipalId` field on `PtySession` / thread / run rows, plus a check in each handler.

---

## 3. Process topology (Proposed, needs sign-off — §12 OD-1)

| | **(A) One privileged broker, per-principal maps** | **(B) Broker + one `ikenga-server` child per principal** |
|---|---|---|
| Shape | One root process serves every principal. `AppState` singletons become `HashMap<PrincipalId, _>`. setuid happens only inside each spawn. | Root broker: auth, accounts, probe, static SPA, reverse proxy (HTTP + WS). It lazily spawns one `ikenga-server` **child per principal, running as that uid**, and proxies that principal's `/api/rpc`, `/ws/*` and `/pkgs/*` to it. The child is today's single-tenant daemon. |
| Blast radius | The whole RPC surface (`fs_*`, settings, supabase, db, pkg) runs **as root**. Only the `fs_roots` allowlist and code stand between a bug and another principal's files, or `/etc/shadow`. | Only auth, proxy and spawn run as root. Every RPC arm runs unprivileged. The **kernel** (0700 dirs, uid) enforces isolation, as the G-73 spike showed (`04` Round 14 `:39`). |
| `AppState` / singletons | Every field in `server/mod.rs:74-107` plus `fs_roots::CURRENT` (`fs_roots.rs:50`) must be keyed. **64** process-env home reads (`platform::home_dir()` / `$HOME`) in `src-tauri/src` need a principal threaded through. | Unchanged. Each child has one `AppState`, one `fs_roots`, one `PaDb`. `HOME`/`USER` in the child's env make `fs_home` (`rpc.rs:418`), `store_root()` (`skill_actions.rs:44`), settings home (`mod.rs:240`), agent-ops (`rpc_local.rs:380`) and `os_username` (`identity.rs:17`) correct **with no code change**. |
| Single-writer migrations | One process opens N `ikenga.db`s; still one writer each, but per-principal pools inside one process. | One child is the only process that ever opens its principal's `ikenga.db`, enforced by an exclusive `flock` (§11.1). The broker opens only `operator/*.db`. |
| PTY setuid | Every PTY spawn must drop inside `portable-pty`. 0.8.1's `CommandBuilder` exposes **no** `pre_exec` hook (its own closure is internal, `portable-pty-0.8.1/src/unix.rs:204`), so A needs a re-exec shim or a fork of portable-pty. | The only setuid spawn is the broker launching the child, through `std`/`tokio` `Command` (§9). PTYs inside the child need no drop. |
| Idle timeout | The global counter (`mod.rs:433-449`) becomes per-principal accounting. | Each child's existing watcher idles it independently. The broker respawns it on the next request. |
| Shared operator assets | The SPA and `--pkgs-dir` are served once. | The broker serves the SPA. Each child walks the same read-only `--pkgs-dir` (`mod.rs:262`), which is operator-owned and world-readable. |
| Cost | Largest refactor, but one process and lowest memory. Cross-principal admin views are trivial. | One process per active principal (idle-reaped), plus a WS-capable reverse proxy in the broker (net-new). Cross-principal admin views go through the broker/accounts DB. |
| Precedent | — | JupyterHub hub + single-user servers; code-server's "one instance per user" (`02` §h `:82`). |

**Recommendation: (B).** It keeps root off the ~85-arm RPC surface. It turns every §5 seam but one (the `service_role_key` policy) into a no-op or a layout change. It makes the single-writer rule structural. It sidesteps portable-pty's missing `pre_exec`. And it confines setuid to one spawn site. The executor seam still earns its keep: the broker's child launch is a `SessionExecutor::spawn_piped` with `SpawnSpec.principal = Some(..)` (§9), and T2 later swaps that one spawn for a container.

**Under B, pinned for the child:** it runs `--executor-tier t1 --principal-child` (a hidden flag). It installs an executor with T0 spawn mechanics whose `probe` verifies the process is **already dropped**: `euid != 0`, `CapEff == CapPrm == 0`, `NoNewPrivs == 1`, uid == the expected uid. It reports `principal_isolation: true` only if all four hold.

The broker ↔ child transport is loopback TCP. The child binds `127.0.0.1:0` and is given a random per-child token through `IKENGA_AUTH_TOKEN`, which `main.rs:114-116` strips from its env. It reports its bound port through its own `<data>/daemon.json` (0600, owned by the uid). A Unix socket under `<data>/run/` is the later hardening. The broker adds `X-Ikenga-Principal` for logging only; the child **never** authorizes on it.

---

## 4. Per-principal data-dir layout (Proposed)

Under T1, `--data-dir` (`server/src/main.rs:33-35`) names the **operator root**, so deploy files keep `/opt/ikenga/data` (`scripts/server/ikenga-server.service:18`, `Dockerfile:46-50`). The tier decides the layout. A T1 boot on a T0-shaped dir (`ikenga.db` present, `operator/` absent) is **refused**, naming the migration command (§11.2).

```
<root>/                                    root:root   0755   (--data-dir)
├── operator/                              root:root   0700
│   ├── accounts.db (+wal/shm)             root        0600   §6.1 — the source of truth for principals
│   ├── sessions.db (+wal/shm)             root        0600   §6.3 — tower-sessions store
│   ├── daemon.json                        root        0600   broker discovery (discovery::write_private)
│   └── probe.json                         root        0600   last §8 report
└── principals/                            root:root   0711   traverse, no listing
    └── <principal_id>/                    uid:gid     0700
        ├── home/                          uid:gid     0700   passwd home: .ikenga/settings.json, Ngwa store
        │                                                     (~/.local/share/app.ikenga/store), .agent-ops,
        │                                                     .atelier, engine logins (.claude, .codex, .gemini)
        └── data/                          uid:gid     0700   the child's --data-dir
            ├── ikenga.db (+wal/shm)       uid         0600   exactly one opener (§11.1)
            ├── .lock                      uid         0600   flock held by the child
            ├── fs_roots.json  supabase.json  daemon.json  screenshot-config.json
            ├── chi-cache/  backups/  tmp/ (TMPDIR)  run/
            └── secrets/                   uid         0700   reserved for WP-21
```

**Stays operator-global:** the static SPA (`--static-dir`), `--pkgs-dir` (root-owned; `0755` dirs, `0644` files; never writable by a principal), the `ikenga-server` binary (root-owned `0755`, non-writable by principals, because the broker execs it as each uid), and `IKENGA_SECRET_*` in the broker's env as the operator default (§5 row 7).

**Home (Proposed — §12 OD-3): a real passwd entry whose home lives on the data volume** (`<id>/home`), not an app-managed dir with no passwd entry, and not `/home/<name>`. Four reasons:
- Engine CLIs and shells need `getpwuid` to resolve. Node's `os.userInfo()` throws when there is no passwd entry, and `whoami`, `ssh` and `git` complain.
- Putting the home on the data volume keeps all principal state on the one persistent volume PaaS gives you.
- On a container redeploy `/etc/passwd` resets with the image, but `accounts.db` survives. So `/etc/passwd` is a **projection** that the broker reconciles from `accounts.db` at every boot (§8 step 7).
- Engine credentials leave the operator's `/root/.claude` / `/root/.gemini` volumes (`Dockerfile:38`) and become per-principal.

---

## 5. The `03` §l seams → T1 fix (under B unless noted)

`03` §l (`plans/remote-access/03-research-internal.md:197-211`) predates WP-19. Rows below are re-anchored on `df5482c` and include every site main now tags `G-PRINCIPAL`. The `WP-20` tags in `engines/openrouter_http`, `claude_store`, `chi.rs:1521`, `db.rs:413` and `skill_actions.rs:546+` belong to **other plans' WP-20s** and are not seams.

| # | Seam | Current code | Assumption | T1 fix |
|---|---|---|---|---|
| 1 | One token, one `AppState` | `server/mod.rs:52-71`, `:74-107`, `:155-208`, `:359-362` | one secret = one owner | Broker: session → `PrincipalCtx` (§2). Child: today's `AppState`, per-child token (§3). |
| 2 | Discovery files | `server/discovery.rs:30-44` is **already per-euid** (`$XDG_RUNTIME_DIR` or `ikenga-daemon-<uid>.json`, 0600, `write_private` `:52`). `data_dir.join("daemon.json")` at `mod.rs:495` | one `daemon.json` per daemon | discovery.rs doesn't change. Broker writes `operator/daemon.json`; each child writes its own `<data>/daemon.json`, and its temp copy lands per-uid on its own. `03` §l's "fixed `$TMP/ikenga-daemon.json`" is **stale**. |
| 3 | `fs_home` | `server/rpc.rs:418-421` | daemon process `HOME` | Child env `HOME=<id>/home`. No code change. |
| 4 | `platform::home_dir()` → settings/agent-ops home | `server/mod.rs:229-241` (`create_router` doc + call), `AppState.home` `:99-104`, `rpc_local.rs:167-178`, `settings/mod.rs:80`, `:96` | process home | As row 3: resolved once per child, correct by construction. |
| 5 | Ngwa store root | `pkg/skill_actions.rs:38-44` (doc + `store_root`); used at `rpc.rs:508-532` | process env | Child env (`HOME`; `XDG_DATA_HOME` unset). |
| 6 | agent-ops job files | `server/shared/agent_ops.rs:14-18`; `rpc_local.rs:377-431` | process home | Child home. `agent_ops_run_now` stays unserved until it spawns through the executor (`desktop_only.toml:101-103`). |
| 7 | `os_username` | `server/shared/identity.rs:13-21`; `rpc_local.rs:438-444`; `rpc.rs:641` | daemon `USER` | Child env `USER=LOGNAME=<unix_name>`. The FE display name should come from `/auth/me.username`, not this. |
| 8 | `fs_roots` process-wide `OnceLock` | `fs_roots.rs:50`, `:200-212`; installed `mod.rs:371-378`; read by `path_allow.rs:86`, `fs_ws.rs:189` | one allowlist per process | One per child from `<data>/fs_roots.json`. Under A: keyed by `PrincipalId` before `resolve_allowlisted`. |
| 9 | Single-writer migrations | `db.rs:604-627` (doc: no txn, no `BEGIN IMMEDIATE`), `:628-668`; `PaDb::new` `:76`; only a warning at `mod.rs:380-395` | one opener per `ikenga.db` | **Rule: no two processes, and no two principals, ever open the same `ikenga.db`.** Enforced by the child's `flock` on `<data>/.lock` before `PaDb` opens (§11.1). The broker never opens a principal DB. `db.rs` stays as is. |
| 10 | `supabase.json` `service_role_key` returned to clients | `server/shared/supabase_config.rs:24-30`; `rpc_local.rs:132-135`; `rpc.rs:611` | token holder = file owner | Per-principal `<data>/supabase.json`. Under B it is only ever returned to its owner, which holds structurally. **Proposed:** no operator-level `service_role_key` is ever seeded into or returned to a principal unless the operator opts in explicitly (`--share-supabase-service-role`); URL + anon key may be seeded. §12 OD-5. |
| 11 | `pkg_index` / `pkg_static` shared | `mod.rs:258-264`; `AppState` `:86-94`; `pkg_index.rs:10`, `:30`; same-origin caveat `pkg_static.rs:41-48` | every token holder sees every pkg | Shared operator-installed set for v1 (§12 OD-10). **Carried risk, for explicit sign-off:** pkg HTML is same-origin with the SPA, so a framed pkg reaches `window.parent` and acts **as the viewing principal**. Pkgs are operator-installed, hence operator-trusted. It is not a cross-principal leak under B, because each principal's SPA only talks to its own child. |
| 12 | PTY / chat registries | `pty/mod.rs:362-363`; `pty_ws.rs:61-99` (auto-spawn `cwd: "."` `:75`); `chat_ws.rs:222-280` | one namespace | Per child (B). Under A: owner field + checks (§2.5). Auto-spawn `cwd "."` becomes the principal's home (§9.3). |
| 13 | Idle timeout | `mod.rs:433-458` counts only PTY sessions (`active_session_count`, `pty/mod.rs:1404`) | daemon-wide clock | Per child. **Pinned:** "active" also counts open WS connections, so an open chat or fs socket isn't reaped. The broker's own process has no idle timeout. |
| 14 | Executor tier + health | `executor/tier.rs:139-151`; `executor/mod.rs:231-262`; `health.rs:45` | T0 only | §8 (probe), §9 (T1 spawn). `principal_isolation: true` only from a verified probe. |
| 15 | `IKENGA_SECRET_*` flat namespace | `secrets_env.rs:43`, `:92-104` | operator-global | No change in WP-20. The broker passes `IKENGA_SECRET_*` to children as the operator-default layer (ADR-023 `:45`). They are already readable by any token holder through the `secrets_get` arm, so this adds no exposure. The PTY denylist keeps them out of shells (`pty/mod.rs:199-204`). WP-21 layers the per-principal store on top. |
| 16 | App-lock / vault unlock | `desktop_only.toml:103-134` (8 `app_lock_*`), `:895-910` (4 `secrets_*` unlock) — 12 tables tagged `WP-20` | one PIN, one DEK | Served per child once WP-21 lands a per-principal store. WP-20 does not unblock these; WP-21 does. |
| 17 | Token env stripping | `server/src/main.rs:94-116` | the daemon's own children | Broker strips as today. The child receives only its own per-child token, which it strips too. §9.2's allowlist is the belt. |

---

## 6. Local-accounts schema (operator DB)

### 6.1 `operator/accounts.db`

This is a separate embedded migration set, **not** `src-tauri/migrations/` (that set is `ikenga.db`'s; its last file is `0068_chi_cache_runner_pid.sql`). It is tracked in `_operator_migrations`. Every migration batch runs inside **one `BEGIN IMMEDIATE` transaction**, so `db.rs`'s race is not repeated. Only the broker migrates. The CLI refuses to run against a schema version it doesn't match.

```sql
CREATE TABLE accounts (
  principal_id        TEXT    PRIMARY KEY
                      CHECK (length(principal_id) = 36 AND principal_id = lower(principal_id)),
  username            TEXT    NOT NULL UNIQUE COLLATE NOCASE
                      CHECK (length(username) BETWEEN 1 AND 32),
  password_phc        TEXT,             -- argon2id PHC string ($argon2id$v=19$m=…,t=…,p=…$salt$hash).
                                        -- NULL = no password login (reserved for WP-22 OIDC-only);
                                        -- WP-20's CLI always sets it.
  unix_name           TEXT    NOT NULL UNIQUE,     -- immutable after provisioning (§7.2)
  unix_uid            INTEGER NOT NULL UNIQUE CHECK (unix_uid > 0),
  unix_gid            INTEGER NOT NULL CHECK (unix_gid > 0),
  home                TEXT    NOT NULL,            -- absolute
  shell               TEXT    NOT NULL DEFAULT '/bin/sh',
  is_admin            INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0, 1)),
  session_epoch       INTEGER NOT NULL DEFAULT 0,  -- bumped to revoke every session (§2.2)
  adopted             INTEGER NOT NULL DEFAULT 0 CHECK (adopted IN (0, 1)),  -- §11.2
  disabled_at         INTEGER,                     -- unix secs; NULL = active
  created_at          INTEGER NOT NULL,
  updated_at          INTEGER NOT NULL,
  password_changed_at INTEGER
);
-- Rows are never deleted (tombstones keep principal_id and unix_uid unreusable).

CREATE TABLE auth_events (                          -- append-only
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  at            INTEGER NOT NULL,
  principal_id  TEXT REFERENCES accounts(principal_id),  -- NULL for an unknown-username failure
  username_tried TEXT,
  kind          TEXT NOT NULL CHECK (kind IN (
                  'login_ok','login_fail','login_throttled','logout','password_changed',
                  'account_created','account_disabled','account_enabled','sessions_revoked',
                  'provision_failed','probe_failed')),
  remote_addr   TEXT,
  user_agent    TEXT,
  detail        TEXT                                -- JSON
);
CREATE INDEX auth_events_principal_at ON auth_events (principal_id, at);
```

`is_admin` is the operator-level "may administer accounts" bit only. G-ACCESS's role set (Admin/Member/Guest, `09-orchestration.md:1105`) may supersede it and must key on `principal_id`. `auth_events` covers authentication only. WP-77's audit log (`plans/shell-ux-rearchitecture/05-tracking.md:1207`) owns the product audit schema and may absorb this table.

**Reserved for WP-22, not created by WP-20:** `oidc_identities(issuer TEXT, subject TEXT, principal_id TEXT NOT NULL REFERENCES accounts, linked_at INTEGER, PRIMARY KEY (issuer, subject))`.

### 6.2 Argon2id (Proposed — §12 OD-8)

Use the already-vendored **`rust-argon2 =2.1.0`** (`Cargo.toml:86`; it is a non-optional dependency, so the server build has it). Its `hash_encoded` / `verify_encoded` produce and verify PHC strings and compare in constant time (`rust-argon2-2.1.0/src/argon2.rs:88`, `:142`, `:200`). One Argon2 implementation stays in the tree.

**Parameters:** Argon2id, v=19, m = 19 456 KiB, t = 2, p = 1, 16-byte random salt, 32-byte hash. These are the same costs as `commands/app_lock.rs:106-109` and `secrets/crypto.rs`. They are carried in the PHC string, so raising them later is rehash-on-next-login.

**Handling:** hashing runs on `spawn_blocking` behind a concurrency cap (4). An unknown username verifies against a fixed dummy hash, so the two cases take the same time. Failed logins back off per username and per remote address using `BACKOFF_STEPS_MS` (`app_lock.rs:100`), and every throttle writes `login_throttled`.

The alternative is RustCrypto `argon2` + `password-hash`, which is what axum-login's examples use. It would be a second Argon2 implementation for no format gain.

### 6.3 Sessions and crate pins

`operator/sessions.db` holds the `tower_sessions` table, created by `SqliteStore::migrate()` as `(id TEXT PK, data BLOB, expiry_date INTEGER)` (`tower-sessions-sqlx-store-0.14.2/src/sqlite_store.rs:58-62`). It is separate from `accounts.db` so per-request session writes don't contend with account writes.

**Pins:**
- `axum-login = "=0.16"`: its manifest requires `axum ^0.7.1` and `tower-sessions ^0.13.0`. 0.18 requires axum 0.8.
- `tower-sessions = "0.13"`.
- `tower-sessions-sqlx-store = { version = "0.14", features = ["sqlite"] }`: 0.14.2 requires `tower-sessions-core ^0.13.0` and `sqlx ^0.8.0`. **0.13 requires core `^0.12.1` and must not be used.**

These match the repo's `axum 0.7.9` / `sqlx 0.8.6` (`Cargo.lock:474-476`, `:6605-6607`). I verified this from the crate manifests. A full `cargo build` against the repo's `tower 0.4` (`Cargo.toml:78`) is **to verify at implementation**.

---

## 7. Provisioning (Proposed)

### 7.1 CLI shape

`ikenga-server` keeps its flat flags as the implicit default (`serve`), so the systemd `ExecStart` and the Docker `ENTRYPOINT` don't change for T0. Subcommands are added (`args_conflicts_with_subcommands`):

```
ikenga-server accounts create  <username> [--admin] [--adopt-unix-user <name>]   # password from TTY or --password-stdin
ikenga-server accounts passwd  <username>                                         # bumps session_epoch
ikenga-server accounts disable <username> | enable <username>
ikenga-server accounts list    [--json]
ikenga-server accounts adopt-t0 --from <old-data-dir> --home <old-home> <username>   # §11.2
ikenga-server probe --executor-tier t1 [--json]                                   # §8, exits 0/1
```

- Every subcommand requires `euid == 0` and `--data-dir`.
- Passwords **never** come from argv; that is G-90's lesson (`04` Round 12 `:133`).
- There is no HTTP route that creates an account in WP-20. **No self-signup.**
- An admin UI or invite flow is G-ACCESS/WP-76's, and must call the same provisioning core.

The CLI writes `accounts.db` directly in `BEGIN IMMEDIATE` transactions. That is safe alongside the running broker, because WAL + immediate transactions serialize writers, and only migrations had the race. The broker re-reads the account row on every request, so a disable takes effect on the next request.

### 7.2 Create: eager

`create` does four things, and a failure at any step rolls the whole create back:
1. It opens `BEGIN IMMEDIATE`.
2. It allocates `unix_uid = max(range_start, MAX(unix_uid over non-adopted rows) + 1)`. Disabled rows count, because uids are never reused. It fails if the result is past `range_end`, or if `getpwuid` / `getgrgid` shows the number already taken by a host user.
3. It mints the UUIDv7, derives `unix_name = "ik-" + lowercase(username)`, validated `^[a-z][a-z0-9-]{0,28}$` (so ≤ 31 chars), and inserts the row.
4. It provisions: user-private group `gid = uid`, a passwd entry (home `<root>/principals/<id>/home`, the shell), then creates and chowns the §4 dirs.

On success it commits. On any failure it rolls back, removes what it created, and records `provision_failed`.

The default uid range is **20000–29999**, set with `--uid-range`. It sits above distro login users and below 60000, and inside the 65 536 ids a user-namespaced container typically maps.

**Provisioning backend (§12 OD-4):** `groupadd`/`useradd` when they are on `PATH`; otherwise a built-in `/etc/group` + `/etc/passwd` (+ `/etc/shadow` with `!`) writer, using `lckpwdf` and a temp-file rename. That is the G-73 `probe.py` `ensure_user` fallback (`plans/remote-access/verify/2026-09-27-g73-railway/probe.py:69`), and it is required by the floor on images with no `passwd` package. A third mode, `--provisioning external`, has the daemon never write `/etc`. The operator pre-creates users, and `create --adopt-unix-user` maps accounts to them.

### 7.3 Disable, enable, passwd

- **Disable** sets `disabled_at`, bumps `session_epoch`, and makes the broker stop the principal's child. It kills **every process of that uid**: a helper spawned through the T1 executor as that uid calls `kill(-1, SIGKILL)`. That covers detached chi-runners in their own process groups. It locks the passwd entry (`!` password, shell `/usr/sbin/nologin`) and **keeps** the files, the uid and the `principal_id`.
- **Enable** reverses the lock and restores the shell.
- There is **no delete in v1**. Purging data is a manual operator act, and the row stays as a tombstone.
- **Passwd** rehashes, sets `password_changed_at`, bumps `session_epoch`, and writes `password_changed`.

### 7.4 First admin (§12 OD-7)

The canonical path is `accounts create --admin` from a host or container shell. For PaaS without a shell there is an env bootstrap: `IKENGA_BOOTSTRAP_ADMIN=<username>` plus `IKENGA_BOOTSTRAP_ADMIN_PASSWORD`. It is honoured **only when `accounts` has zero rows**, and otherwise ignored with a warning. Both vars are captured into config and `remove_var`'d next to `main.rs:114-116`. A consumed bootstrap is logged without the password.

---

## 8. Boot probe (T1)

`probe(T1)` runs first in `run_server`, before anything binds or is written (`server/mod.rs:334-349`). Any failure is `Refusal::ProbeFailed { check, detail }`, and the daemon **does not start** (DEC-R9-1). There is never a fallback to T0.

| # | Check | Pass condition | Notes |
|---|---|---|---|
| 1 | OS | `target_os = "linux"` | T1 is Linux-only. |
| 2 | Identity | `/proc/self/status` `Uid:` shows euid `0` | The broker needs `useradd`/chown/kill across uids. Capabilities without root isn't a supported mode in v1 (§12 OD-1 note). |
| 3 | Capabilities | `CapEff` has bit **7** (`CAP_SETUID`), **6** (`CAP_SETGID`), **0** (`CAP_CHOWN`), **5** (`CAP_KILL`) | Decoded from the hex mask, as `probe.py:52-61` does. Railway's `0x800405fb` passes (`04` Round 14 `:37`). |
| 4 | Observations | Record `NoNewPrivs`, `Seccomp` and the filter count | Not a gate: NNP=1 on the broker does not block `setuid()` by a `CAP_SETUID` holder. The real drop (6) is the proof. Railway: NNP 0, seccomp 2 / 3 filters, drop fine (`:41`). |
| 5 | Operator root | `<root>`, `operator/`, `principals/` exist or can be created with §4's owners and modes; none is a symlink or group/world-writable; `<root>` is not a T0 layout | |
| 6 | **Real test drop** | The broker spawns **itself** (`/proc/self/exe __t1-probe-child`) **through the T1 executor** (§9) as the probe principal: uid = gid = `range_end`, no passwd entry needed, home = a fresh root-created `0700` probe dir chowned to that uid. The child must: see `getresuid` / `getresgid` all equal to the probe ids and `getgroups()` empty; get `EPERM` from `setuid(0)`; get `EACCES` opening a root-owned `0700` dir; write a file in its dir whose `st_uid` the parent then confirms is the probe uid; see `NoNewPrivs: 1` if OD-9 = yes. It exits `0`; the parent checks the exit code, removes the probe dir, and times out at 10 s. | This runs the **same code path real spawns use**, not a parallel `fork()`. That avoids fork-in-a-multithreaded-runtime hazards and covers gVisor/fakeroot-style cosmetic uid changes. |
| 7 | Reconcile | For every non-disabled row, the passwd/group entry matches `(unix_name, uid, gid, home, shell)`. Missing entries are (re)created with the §7.2 backend. | Refuse if a row's uid or name is held by a **different** host entry. `/etc` is a projection of `accounts.db` (§4). |

**Results.**
- **Health:** `/api/health` is unauthenticated, so it carries only non-sensitive fields: the `Capabilities` shape (`tier.rs:86-96`) unchanged, plus `probe: { ok: true, at: <unix secs> }`. No uids, names, caps masks or paths. The full report goes to the log and `operator/probe.json`.
- **Runtime drift:** a spawn-time drop verification failure (§9.2) marks the executor **degraded**. Every later T1 spawn is refused, and health flips `principal_isolation: false` until restart. Fail closed.
- **`ikenga-server probe`** (Proposed) runs steps 1–6 read-only (no reconcile), prints the report and exits 0/1. It is the Rust successor of the G-73 script, for operators and for re-probing Render.
- **Deploy consequence:** the shipped unit runs `User=ikenga` with `ProtectSystem=strict` (`ikenga-server.service:7`, `:57`). Both fail steps 2–3 and 7 as written. WP-20 ships a T1 variant: root, `ReadWritePaths=` the operator root plus `/etc/{passwd,group,shadow,gshadow}` (or `--provisioning external`), and `CapabilityBoundingSet=CAP_SETUID CAP_SETGID CAP_CHOWN CAP_KILL CAP_DAC_OVERRIDE CAP_FOWNER`. `NoNewPrivileges=true` can stay.

---

## 9. Setuid spawn contract (`T1Executor`)

### 9.1 Resolution (before fork)

`SpawnSpec.principal = Some(p)` has already been resolved by the caller from `accounts.db` to a complete `Principal` (§1). Under B that caller is the broker's child launcher. The executor builds every `CString` path, argv and envp **before** the spawn. Nothing in the child between fork and exec allocates, locks, logs or reads NSS. Under T1, `principal = None` is a **refusal** for every spawn except the broker's own internal probe: nothing a session asks for runs as root.

### 9.2 Order in the child

Use `std`/`tokio` `CommandExt::gid(p.gid)` + `uid(p.uid)`. The required order is `setgroups([])` → `setgid` → `setuid`, all before `exec`. When the parent is uid 0, std performs `setgid`, then `setgroups(0, NULL)`, then `setuid`, then `chdir(cwd)` as the new uid, and only then runs `pre_exec` closures. (This ordering comes from memory of std's `do_exec` and is *to verify at implementation*.) std **ignores** a `setgroups` failure, so the one `pre_exec` closure **verifies instead of trusting**; if the ordering ever differs, the checks fail closed:
1. `getresuid` / `getresgid` all equal `(uid, gid)`.
2. `getgroups(0, NULL) == 0`.
3. `setuid(0)` returns `-1`/`EPERM`.
4. `prctl(PR_SET_NO_NEW_PRIVS, 1)` (OD-9).
5. Any failure → return `Err`, so the spawn fails.

`spawn_pty` under T1 with a principal is **unsupported under B** (PTYs live in the child). Under A it needs a re-exec shim: `ikenga-server __exec-as` performs 9.2 and then `execvp`s, because portable-pty 0.8.1 has no hook (§3). Chowning the slave pty to the uid, mode 0620, is part of that shim.

### 9.3 Environment and cwd

- **Env:** `env_clear`, then exactly `HOME=p.home`, `USER=LOGNAME=p.unix_name`, `SHELL=p.shell`, `PATH=<operator --principal-path, default p.home/.local/bin:/usr/local/bin:/usr/bin:/bin>`, `TMPDIR=<data>/tmp` (because `/tmp` is shared across principals), and `LANG`/`LC_*`/`TZ` passed through. Then the spec's own vars.
- **Deny floor:** the T1 executor **drops** any spec var matching `is_host_only_env` (`pty/mod.rs:199-204`: `IKENGA_AUTH_TOKEN`, `IKENGA_VAULT_KEY`, `IKENGA_PKG_DB_TOKEN`, `IKENGA_SECRET_*`), plus `IKENGA_BOOTSTRAP_*`. The child launcher's per-child token and `IKENGA_SECRET_*` defaults (§5 row 15) are the only exception, and they are listed explicitly. This **amends** `EnvSpec`'s "the executor does not second-guess" (`executor/mod.rs:64-66`) for T1 only: the floor is the executor's job there, because a call site's filter is not a security boundary.
- **cwd:** `None` → `p.home`. The T1 executor never inherits the broker's cwd, which is where `pty_ws.rs:75`'s `"."` would otherwise land.

### 9.4 Detached chi-runner under T1

Under B the child spawns chi-runner detached as the uid (`commands/chi_runner.rs:97`, `PipedOpts::detached` `executor/mod.rs:180-191`). Its conf and status files live in `<data>/chi-cache/`, owned by the uid. The runner survives the child's idle exit. A broker **restart** under systemd still kills it unless the unit fix recorded at `scripts/server/ikenga-server.service:45-54` is applied (`systemd-run --scope` per runner, or `KillMode=process`; `04` Round 12 `:137`). That fix is owed by the T1 unit variant (§8). Cancel and disable reach the runner by process group (`kill_process_group`, `commands/chi_runner.rs:121`) and by the §7.3 uid-wide kill.

### 9.5 Single interposition point

Every desktop session spawn goes through `executor::current()` once #305 merges. Until then the pkg, install, cron, playwright, `action_exec` and `agent_detect` sites call `Command::new` directly (`pkg/mcp_runtime.rs`, `pkg/lifecycle.rs`, `pkg/registries/cron.rs`, …). Under B those run inside the unprivileged child, so they are isolated anyway. Under A, **T1 must not ship before #305**. #305's deliberate exclusions (`path_fix.rs`, `runtime.rs` boot probe, `pty/daemon_client.rs`, desktop-only-forever) are host-process self-maintenance or desktop-only, and none runs a session.

---

## 10. Consumers

| Consumer | Needs from this contract |
|---|---|
| **WP-20** | Implements §1–§9 and §11. Owns the broker, accounts, probe, T1 executor and FE login mode. |
| **WP-21** | Keys the per-principal `SecretsStore` on `PrincipalId`, stored under `<data>/secrets/`. Layer order: principal store → `IKENGA_SECRET_*` operator default (ADR-023 `:45`). Under B the store lives in the child, as the uid. A server-held KEK would have to be handed to the child by the broker. That is WP-21's envelope decision (`02` §k), made with this constraint in view. |
| **WP-22** | `oidc_identities (issuer, subject) → principal_id` (§6.1). OIDC login produces the **same** session cookie and `PrincipalCtx`. **No auto-provisioning** of unknown identities unless the founder opts in; an unknown `sub` is refused. |
| **WP-73 (G-ACCESS)** | Roles, capabilities, devices and audit records key on `principal_id`, never username or uid. Whether a device is a child of a principal or a first-class identity is G-ACCESS's call, but a device credential must resolve to exactly one `principal_id` (§2.1). |
| **WP-74** | Device grants are a `Credential::DeviceGrant` resolving to a principal. Under T1 the bearer is already retired (§2.4). Revocation must be as immediate as `session_epoch`. |
| **WP-76 / WP-77** | Invites and member management call §7's provisioning core. Whether invite acceptance counts as "admin-created" needs its own Round (D3 forbids self-signup). WP-77 may absorb `auth_events`. D-05 sign-in UI: see OD-12. |

---

## 11. Invariants, migration, non-goals

### 11.1 Invariants (each gets a test)

- **I-1** Every spawn under T1 carries a resolved `Principal`, with `uid != 0` and `gid != 0`. Otherwise it is refused (probe excepted).
- **I-2** After any T1 spawn, the child cannot regain root: `setuid(0)` fails, groups are empty, and NNP=1 if OD-9 = yes.
- **I-3** No two processes, and no two principals, ever open the same `ikenga.db`. A second child for the same principal fails its `flock` and exits.
- **I-4** `principal_id` and `unix_uid` are never reused. `accounts` rows are never deleted.
- **I-5** `/api/health` reports `principal_isolation: true` only if the §8 probe passed in this process and the executor is not degraded.
- **I-6** Under T1, no request reaches an RPC/WS/pkg handler without a `PrincipalCtx`. `?token=` and the operator bearer grant nothing.
- **I-7** A principal cannot resolve, attach to, signal or read another principal's PTY, thread, run, watch or files. Under B this is enforced by process and uid; under A, by owner checks.
- **I-8** Bumping `session_epoch` (passwd / disable) invalidates every existing session of that principal on its next request.
- **I-9** `<root>/principals/<id>` and everything under it is owned by that uid with no group/other bits. `operator/` is root `0700`. Nothing a principal owns is ever executed by root.
- **I-10** A T1 boot on a T0-shaped data dir is refused, never auto-migrated.

### 11.2 Migration: T0 install → T1

Either way the operator stops the T0 daemon first. `adopt-t0` refuses while `<old>/daemon.json`'s pid is alive.

- **Systemd/VPS T0 (runs as `User=ikenga`):** `accounts create <username> --admin --adopt-unix-user ikenga` maps the first principal onto the **existing** uid and home (`adopted = 1`; the uid may lie outside the range). Then `adopt-t0 --from /opt/ikenga/data` moves the flat data dir into `principals/<id>/data/`: rename on the same filesystem, else copy+verify. It sets `home` to the existing home. Paths recorded in `ikenga.db` and `fs_roots.json` stay valid, because the home didn't move. The old dir is kept as `<old>.t0-migrated-<ts>`, read-only.
- **Docker/PaaS T0 (runs as root, `/root/.claude` volumes, `Dockerfile:38`):** root can't be adopted (I-1). So `adopt-t0` creates a fresh principal, copies the data dir, and copies `.ikenga`, `.local/share/app.ikenga`, `.agent-ops`, `.atelier`, `.claude`, `.codex`, `.gemini` from the old home into `<id>/home`. It chowns everything recursively and rewrites `fs_roots.json` entries under the old home prefix. Other absolute paths in `ikenga.db` are **not** rewritten; they are listed in the migration report.
- Chi runs that were still live under T0 are reconciled as `Unverified` by the existing sweep, because the pid probe runs as a different uid now.

### 11.3 Non-goals

Self-signup; LDAP/AD; OIDC flows (WP-22); per-principal pkg install or trust (v1); T2/T3; non-root operation with file capabilities or a sudo helper (SudoSpawner-style, `02` §h — possible later, not v1); account deletion or uid reuse; changing the T0 auth model; Windows/macOS T1.

---

## 12. Open decisions for founder sign-off

### 12.1 Decisions

| # | Question | Options | Recommendation | Consequence |
|---|---|---|---|---|
| **OD-1** | Process topology | (A) one privileged broker, per-principal maps · (B) broker + per-principal child as uid | **B** (§3) | B: root touches only auth/proxy/spawn, and the seams dissolve. The cost is a WS reverse proxy and one process per active user. A: every RPC arm runs as root, 64 home reads get re-plumbed, and PTY setuid needs a shim. |
| **OD-2** | Principal id encoding | UUIDv7 · ULID · uid · username | **UUIDv7** (§1) | No new crate; stable across rename and re-provision; OIDC/WP-21/G-ACCESS key on it. |
| **OD-3** | Home | real passwd entry, home on the data volume · app-managed dir with no passwd entry · `/home/<name>` | **Real entry, home under `<id>/home`**, reconciled at boot (§4) | CLIs that call `getpwuid` work, and state survives PaaS redeploys. It needs `/etc` writes (or `external` mode). |
| **OD-4** | Provisioning backend | `useradd` only · built-in writer only · `useradd` then built-in fallback · `external` | **`useradd` → built-in fallback; `external` opt-in** (§7.2) | Works on slim/Alpine/distroless images without host packages (the floor). |
| **OD-5** | `service_role_key` | return to owner only · withhold always · operator default shared | **Per-principal, owner-only (structural under B); the operator's key is never shared unless `--share-supabase-service-role`** | Principals bring their own key, or get anon-key access only. |
| **OD-6** | Bearer token under T1 | retire for principal surfaces · keep as an operator super-credential · per-principal PATs now | **Retire under T1; T0 unchanged; PATs/devices are WP-74's** (§2.4) | No network credential grants every principal. CLI clients log in with a cookie. |
| **OD-7** | First-admin bootstrap | CLI only · env only · both | **Both: CLI canonical; env honoured only on an empty `accounts`** (§7.4) | PaaS without a shell can bootstrap. The env path is one-shot. |
| **OD-8** | Argon2 crate | vendored `rust-argon2 =2.1.0` · RustCrypto `argon2` + `password-hash` | **rust-argon2** (§6.2) | One implementation in tree; PHC strings either way. |
| **OD-9** | `PR_SET_NO_NEW_PRIVS` on T1 children | yes · no | **Yes** | Blocks regaining privilege through setuid binaries (`sudo`, `su`, `ping`) inside a principal's shell. The cost: those binaries stop working in T1 shells. |
| **OD-10** | Pkg visibility per principal | shared operator-installed set · per-principal enable/trust | **Shared for v1.** ADR-023 frames T1 as a "multi-user, mutually trusted team" (`:37`; `04` Round 13 `:61`). | Sign off the same-origin carry-over (§5 row 11) explicitly. Per-principal enable is a later additive column. |
| **OD-11** | Local accounts under T0 | T1 only · allow one account as a bearer replacement on T0 | **T1 only** (schema: `unix_uid NOT NULL`) | T0 stays bearer-only. Accounts appear with the probe that makes "one account = one uid" true. The alternative makes `unix_*` nullable and changes I-1. |
| **OD-12** | Who builds the login screen | WP-20 ships a minimal login form + reauth mode · wait for D-05 sign-in states (WP-76 / G-98) | **WP-20 minimal**, restyled later by WP-76 | Without it T1 is unusable until wave 14c. Needs a shell-ux Round to note the overlap with G-98. |
| **OD-13** | Child lifecycle | idle-reap the child (default 30 min, no PTY and no WS) · keep it alive while the broker lives | **Idle-reap; respawn on the next request** | Memory scales with active users. Detached chi-runs survive the reap. |
| **OD-14** | T0 → T1 migration | adopt the existing Unix user · always copy into a fresh principal | **Adopt where T0 ran as non-root; copy for root/Docker** (§11.2) | No path rewriting on VPS installs. |

### 12.2 Pins made inside WP-20's remit

| Pin | Choice | Reason |
|---|---|---|
| **P-1** | `--data-dir` = the operator root under T1 | Deploy files stay unchanged. |
| **P-2** | uid range 20000–29999 (`--uid-range`); `gid = uid` | Clear of login users and inside the 65 536 ids a user namespace maps. |
| **P-3** | Cookie `ikenga_session`; `Secure` on by default; `--insecure-cookie` opt-out for a plain-HTTP tailnet | Tailnet deploys serve HTTP on a tailnet IP (`ikenga-server.service:10-13`, `:34-36`). |
| **P-4** | Session inactivity 24 h; the session id cycles on login | The session is a shell credential. |
| **P-5** | `unix_name = "ik-" + username`, immutable | Readable in `ps`; renames never touch `/etc`. |
| **P-6** | `accounts.db` migrations in `BEGIN IMMEDIATE`; `sessions.db` separate | Doesn't repeat `db.rs:604-627`. |
| **P-7** | Broker ↔ child over loopback + per-child token; UDS later | Reuses today's child auth unchanged. |

---

## 13. Sign-off

| Condition | State |
|---|---|
| Contradictions review by a separate reviewer | **not yet run** |
| OD-1…OD-14 answered | **open** |
| Locked decisions (§0.1) re-opened? | **No** |
| Depends on #305 merging | Only if OD-1 = A (§9.5) |
| Recorded in `plans/remote-access/04-discussion.md` | **pending**: the freeze gets its own Round, and shell-ux notes it in its next Round (G-ACCESS waits on this, `plans/shell-ux-rearchitecture/04-discussion.md:147-149`) |

After sign-off, any change to §1–§9 or §11.1 needs a new Round in `04`.

**Status: DRAFT — freeze gate G-PRINCIPAL, awaiting founder sign-off. Not to be implemented until signed.**
