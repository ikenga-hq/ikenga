#!/bin/bash
# Run the Postgres backups of one schedule: pg_dump -> gzip -> verify -> upload.
#
#   run-backup.sh --schedule <name>     back up every enabled database of that schedule
#   run-backup.sh --init-status         create/refresh status.json from the config (no dumps)
#   run-backup.sh --install-key         write the GCS key (from stdin) to the key file, atomically, mode 0600
#   run-backup.sh --mark-disabled       set enabled=false in status.json
#
# The last three are how provision.sh (root) touches the backup user's
# directories: it runs this script AS the backup user, so root never follows a
# symlink in a directory the user controls.
#
# Ported from the rex-vps db-backup scripts (D-B10) and installed by
# `provision.sh backups` into /usr/local/lib/ikenga-backup. systemd runs it as
# the dedicated backup user (unit ikenga-backup@<schedule>.service); nothing
# else should. What changed from rex-vps, and why:
#
#   * No `source .env`. The connection strings live in a root-owned 0640 file
#     that this script parses line by line (NAME=value) into shell variables.
#     It never exports them, never evals them, never puts one in argv.
#   * pg_dump gets the connection through PG* environment variables, parsed from
#     the URL (see parse_pg_url), not as `pg_dump <url>`: a URL in argv is
#     readable by every user on the box through /proc/<pid>/cmdline.
#   * The dump is streamed into gzip (no uncompressed copy on disk), and the
#     pipeline's exit status is checked for BOTH halves.
#   * Each database uses its own gcs_bucket (rex-vps used the first one for all).
#   * One database failing never stops the others; the run exits 1 if any failed.
#   * A status file (STATE/status.json, world-readable, no secrets) is updated
#     after every database. It is what Rex's alert schedule reads.
#   * The journal gets database NAMES and error KINDS only. The raw tool output
#     (which can carry hostnames and key ids) goes to a 0600 file in the backup
#     user's private directory, with the connection strings scrubbed out.
#
#   * Authentication is pinned for a database reached through an SSH tunnel.
#     The tunnel's local end (127.0.0.1:<port>) is a plain TCP port that, while
#     the tunnel is down, any local account could bind; a fake server there would
#     happily ask for a cleartext password. So libpq is told require_auth: the
#     per-database "require_auth" of the config (PGREQUIREAUTH in the pg_dump
#     environment), defaulting to scram-sha-256 when the connection string points
#     at loopback and one of the config's "tunnel_ports". With it libpq refuses
#     to answer a cleartext/md5 request and refuses a server that never
#     authenticated, so the password is not sent and no forged dump is accepted.
#     "require_auth": false in the config opts one database out.
#
# Error kinds (status.json "last_error_kind"): bad-config no-secret
# bad-connection-string no-pg-dump dump-failed auth-refused verify-failed
# gcs-auth upload-failed.
set -uo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG="${BACKUP_CONFIG_FILE:-/etc/ikenga-backup/backup-config.json}"
ENVFILE="${BACKUP_ENV_FILE:-/etc/ikenga-backup/connections.env}"
STATE="${BACKUP_STATE_DIR:-/var/lib/ikenga-backup}"
STATUS_DIR="${BACKUP_STATUS_DIR:-$STATE/status}"      # the backup user's; STATE itself is root's
PRIV="${BACKUP_HOME:-$STATE/private}"
KEY_FILE="${BACKUP_KEY_FILE:-$PRIV/gcs-key.json}"
WORK_ROOT="${BACKUP_WORK_DIR:-$PRIV/work}"
ERR_DIR="$PRIV/errors"
DUMP_MIN_MAJOR="${BACKUP_PG_MIN_MAJOR:-17}"
VERIFY="${BACKUP_VERIFY:-$SELF_DIR/verify-backup.sh}"
STATUS="$STATUS_DIR/status.json"

export CLOUDSDK_CORE_DISABLE_PROMPTS=1
export CLOUDSDK_CORE_DISABLE_USAGE_REPORTING=true
export CLOUDSDK_COMPONENT_MANAGER_DISABLE_UPDATE_CHECK=1

SCHEDULE=""
MODE=run
while [[ $# -gt 0 ]]; do
  case "$1" in
    --schedule) SCHEDULE="${2:-}"; shift 2 ;;
    --init-status) MODE=init; shift ;;
    --install-key) MODE=install-key; shift ;;
    --mark-disabled) MODE=mark-disabled; shift ;;
    *) echo "ikenga-backup: unknown argument" >&2; exit 2 ;;
  esac
done
if [[ "$MODE" == run ]]; then
  [[ "$SCHEDULE" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]] || { echo "ikenga-backup: --schedule <name> is required (lowercase letters, digits, -)" >&2; exit 2; }
fi

# One gcloud config directory PER SCHEDULE (the unit sets it): gcloud keeps its
# credentials in sqlite files there, and schedules that start together (a
# Persistent timer catching up after downtime fires them all at once) must not
# activate into the same one.
export CLOUDSDK_CONFIG="${CLOUDSDK_CONFIG:-$PRIV/gcloud/${SCHEDULE:-default}}"

say() { printf 'ikenga-backup: %s\n' "$*"; }
now() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# ------------------------------------------------------------------ status

# status_edit <jq filter> [jq args...]: read-modify-write STATUS under a lock,
# atomically (temp file in the same directory, then rename), mode 0644.
status_edit() {
  local filter="$1"; shift
  (
    flock -x 9
    local cur='{}' tmp
    [[ -s "$STATUS" ]] && cur="$(cat -- "$STATUS" 2>/dev/null || true)"
    jq -e 'type == "object"' <<<"$cur" >/dev/null 2>&1 || cur='{}'
    tmp="$(mktemp "$STATUS_DIR/.status.XXXXXX")" || exit 1
    if jq "$@" "$filter" <<<"$cur" >"$tmp"; then
      chmod 0644 "$tmp"; mv -f -- "$tmp" "$STATUS"
    else
      rm -f -- "$tmp"; exit 1
    fi
  ) 9>>"$STATUS_DIR/.status.lock"
}

BLANK_DB='{schedule:null,last_attempt:null,last_attempt_ok:null,last_success:null,last_error_kind:null,last_error_at:null,object:null,bytes:null,duration_s:null}'

# The databases this config would back up, as [{name,schedule}].
config_dbs() {
  jq -c '[.databases[] | select(.enabled == true) | {name, schedule}]' "$CONFIG"
}

init_status() {
  local dbs; dbs="$(config_dbs)" || return 1
  status_edit "
    .schema = 1 | .enabled = true | .updated = \$now
    | (.databases // {}) as \$old
    | .schedules = (.schedules // {})
    | .databases = ([\$dbs[] | {key: .name, value: ((\$old[.name] // $BLANK_DB) + {schedule: .schedule})}] | from_entries)
  " --arg now "$(now)" --argjson dbs "$dbs"
}

record_db() {   # name schedule ok kind object bytes duration
  status_edit "
    .schema = 1 | .enabled = true | .updated = \$now
    | .databases = (.databases // {})
    | .databases[\$n] = (((.databases[\$n] // $BLANK_DB)) + {schedule: \$s, last_attempt: \$now, last_attempt_ok: (\$ok == \"true\"), duration_s: \$dur}
        + (if \$ok == \"true\" then {last_success: \$now, last_error_kind: null, object: \$obj, bytes: \$bytes}
           else {last_error_kind: \$kind, last_error_at: \$now} end))
  " --arg now "$(now)" --arg n "$1" --arg s "$2" --arg ok "$3" --arg kind "$4" --arg obj "$5" \
    --argjson bytes "${6:-0}" --argjson dur "${7:-0}"
}

record_schedule() {   # schedule succeeded failed-names-json
  status_edit "
    .schedules = (.schedules // {})
    | .schedules[\$s] = {last_run: \$now, last_run_ok: ((\$failed | length) == 0), succeeded: \$ok, failed: \$failed}
    | .updated = \$now
  " --arg now "$(now)" --arg s "$1" --argjson ok "$2" --argjson failed "$3"
}

# ------------------------------------------------------------ connections

declare -A CONN=()
CONN_READABLE=0
load_connections() {
  [[ -r "$ENVFILE" ]] || return 0
  CONN_READABLE=1
  local l
  while IFS= read -r l || [[ -n "$l" ]]; do
    [[ -z "$l" || "$l" == '#'* || "$l" != *=* ]] && continue
    CONN["${l%%=*}"]="${l#*=}"
  done <"$ENVFILE"
}

# percent-decode $1 into the variable named by $2 (a plain loop: printf %b would
# also interpret backslashes in a password). Fails on %00.
pct_decode() {
  local s="$1" out="" hex
  while [[ "$s" == *%* ]]; do
    out+="${s%%%*}"; s="${s#*%}"
    [[ "$s" =~ ^([0-9A-Fa-f]{2}) ]] || return 1
    hex="${BASH_REMATCH[1]}"; s="${s:2}"
    [[ "${hex^^}" != 00 ]] || return 1
    printf -v hex '%b' "\\x$hex"
    out+="$hex"
  done
  printf -v "$2" '%s' "$out$s"
}

# parse_pg_url <url>: fill CONNV (libpq environment variables) from a
# postgres:// or postgresql:// URL. On failure sets PARSE_ERR to a message with
# no part of the URL in it.
declare -A CONNV=()
PARSE_ERR=""
parse_pg_url() {
  local url="$1" rest auth hostport path query userinfo host port user pass db kv k v
  CONNV=(); PARSE_ERR=""
  case "$url" in
    postgres://*) rest="${url#postgres://}" ;;
    postgresql://*) rest="${url#postgresql://}" ;;
    *) PARSE_ERR="not a postgres:// or postgresql:// URL"; return 1 ;;
  esac
  [[ "$rest" != *$'\n'* && "$rest" != *' '* ]] || { PARSE_ERR="whitespace in the URL"; return 1; }
  query=""; [[ "$rest" == *\?* ]] && { query="${rest#*\?}"; rest="${rest%%\?*}"; }
  rest="${rest%%#*}"
  auth="${rest%%/*}"; path=""; [[ "$rest" == */* ]] && path="${rest#*/}"
  hostport="${auth##*@}"; userinfo=""; [[ "$auth" == *@* ]] && userinfo="${auth%@*}"
  if [[ "$hostport" =~ ^\[([0-9A-Fa-f:.]+)\](:([0-9]{1,5}))?$ ]]; then
    host="${BASH_REMATCH[1]}"; port="${BASH_REMATCH[3]}"
  elif [[ "$hostport" =~ ^([^]:/[]*)(:([0-9]{1,5}))?$ ]]; then
    host="${BASH_REMATCH[1]}"; port="${BASH_REMATCH[3]}"
  else
    PARSE_ERR="cannot parse the host/port"; return 1
  fi
  if [[ -n "$userinfo" ]]; then
    user="${userinfo%%:*}"; pass=""; [[ "$userinfo" == *:* ]] && pass="${userinfo#*:}"
    pct_decode "$user" user || { PARSE_ERR="bad percent-encoding in the user"; return 1; }
    pct_decode "$pass" pass || { PARSE_ERR="bad percent-encoding in the password"; return 1; }
    [[ -z "$user" ]] || CONNV[PGUSER]="$user"
    [[ -z "$pass" ]] || CONNV[PGPASSWORD]="$pass"
  fi
  [[ -z "$host" ]] || CONNV[PGHOST]="$host"
  [[ -z "$port" ]] || CONNV[PGPORT]="$port"
  if [[ -n "$path" ]]; then
    pct_decode "$path" db || { PARSE_ERR="bad percent-encoding in the database name"; return 1; }
    CONNV[PGDATABASE]="$db"
  fi
  if [[ -n "$query" ]]; then
    local -a kvs; IFS='&' read -ra kvs <<<"$query"
    for kv in "${kvs[@]}"; do
      [[ -n "$kv" ]] || continue
      k="${kv%%=*}"; v=""; [[ "$kv" == *=* ]] && v="${kv#*=}"
      pct_decode "$v" v || { PARSE_ERR="bad percent-encoding in parameter $k"; return 1; }
      case "$k" in
        sslmode) CONNV[PGSSLMODE]="$v" ;;
        sslrootcert) CONNV[PGSSLROOTCERT]="$v" ;;
        connect_timeout) CONNV[PGCONNECT_TIMEOUT]="$v" ;;
        channel_binding) CONNV[PGCHANNELBINDING]="$v" ;;
        require_auth)
          require_auth_ok "$v" || { PARSE_ERR="require_auth is not a libpq list such as scram-sha-256"; return 1; }
          CONNV[PGREQUIREAUTH]="$v" ;;
        application_name) CONNV[PGAPPNAME]="$v" ;;
        *) PARSE_ERR="unsupported URL parameter '$k' (supported: sslmode sslrootcert connect_timeout channel_binding require_auth application_name)"; return 1 ;;
      esac
    done
  fi
  [[ -n "${CONNV[PGHOST]:-}" ]] || { PARSE_ERR="no host in the URL"; return 1; }
  [[ -n "${CONNV[PGDATABASE]:-}" ]] || { PARSE_ERR="no database name in the URL"; return 1; }
  : "${CONNV[PGCONNECT_TIMEOUT]:=30}"
  : "${CONNV[PGAPPNAME]:=ikenga-backup}"
  return 0
}

# libpq require_auth: a comma list of password md5 gss sspi scram-sha-256 none,
# each optionally negated with !, all negated or none (libpq refuses a mix).
REQUIRE_AUTH_RE='^!?(password|md5|gss|sspi|scram-sha-256|none)(,!?(password|md5|gss|sspi|scram-sha-256|none))*$'
require_auth_ok() {
  [[ "$1" =~ $REQUIRE_AUTH_RE ]] || return 1
  local IFS=,; local -a parts=($1); local neg=0 p
  for p in "${parts[@]}"; do [[ "$p" == '!'* ]] && neg=$((neg + 1)); done
  [[ $neg -eq 0 || $neg -eq ${#parts[@]} ]]
}

# Replace each secret in stdin with ***. Used before any tool output is kept.
scrub() {
  local line s
  while IFS= read -r line || [[ -n "$line" ]]; do
    for s in "$@"; do [[ -n "$s" ]] && line="${line//"$s"/***}"; done
    printf '%s\n' "$line"
  done
}

pick_pg_dump() {
  local best="" best_major=0 p m
  for p in /usr/lib/postgresql/*/bin/pg_dump; do
    [[ -x "$p" ]] || continue
    m="${p#/usr/lib/postgresql/}"; m="${m%%/*}"
    [[ "$m" =~ ^[0-9]+$ ]] || continue
    (( m > best_major )) && { best="$p"; best_major="$m"; }
  done
  if [[ -z "$best" ]] && command -v pg_dump >/dev/null 2>&1; then
    best="$(command -v pg_dump)"
    best_major="$("$best" --version 2>/dev/null | grep -oE '[0-9]+' | head -1 || echo 0)"
  fi
  DUMP_BIN="$best"; DUMP_MAJOR="${best_major:-0}"
  [[ -n "$DUMP_BIN" ]] && (( DUMP_MAJOR >= DUMP_MIN_MAJOR ))
}

# ---------------------------------------------------------------- one db

# The local ends of the box's SSH tunnels (config "tunnel_ports", written by
# provision.sh), as " 5544 5545 ".
TUNNEL_PORTS=" "
load_tunnel_ports() {
  TUNNEL_PORTS=" $(jq -r '(.tunnel_ports // []) | map(select(type == "number") | floor | tostring) | join(" ")' "$CONFIG" 2>/dev/null || true) "
}

# backup_one <name> <connection secret name> <bucket> [require_auth: "" = default, "@off" = none, else a list]
# Sets KIND (error kind on failure), OBJECT and BYTES (on success). Returns 0/1.
KIND=""; OBJECT=""; BYTES=0
backup_one() {
  local name="$1" secret="$2" bucket="$3" reqauth="${4:-}"
  KIND=""; OBJECT=""; BYTES=0
  local errf="$ERR_DIR/last-error-$name.log" url="" out ts year month

  [[ "$name" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$ && "$secret" =~ ^[A-Za-z_][A-Za-z0-9_]*$ && "$bucket" =~ ^[a-z0-9][a-z0-9._-]{1,220}$ ]] \
    || { KIND=bad-config; return 1; }
  : >"$errf"; chmod 0600 "$errf"

  url="${CONN[$secret]:-}"
  [[ -n "$url" ]] || { KIND=no-secret; echo "connection secret $secret is not in the connections file" >"$errf"; return 1; }
  parse_pg_url "$url" || { KIND=bad-connection-string; echo "$PARSE_ERR" >"$errf"; return 1; }
  case "$reqauth" in
    ""|@off) ;;
    *) require_auth_ok "$reqauth" || { KIND=bad-config; echo "require_auth is not a libpq list such as scram-sha-256" >"$errf"; return 1; } ;;
  esac
  # The config's require_auth wins over a weaker one in the URL. Absent, a
  # connection that ends at one of the box's tunnel ports gets scram-sha-256.
  if [[ -n "$reqauth" && "$reqauth" != @off ]]; then
    CONNV[PGREQUIREAUTH]="$reqauth"
  elif [[ -z "$reqauth" && -z "${CONNV[PGREQUIREAUTH]:-}" \
          && "${CONNV[PGHOST]}" =~ ^(127(\.[0-9]{1,3}){3}|localhost|::1)$ \
          && "$TUNNEL_PORTS" == *" ${CONNV[PGPORT]:-5432} "* ]]; then
    CONNV[PGREQUIREAUTH]="scram-sha-256"
  fi
  [[ -n "$DUMP_BIN" ]] || { KIND=no-pg-dump; echo "no pg_dump >= $DUMP_MIN_MAJOR installed" >"$errf"; return 1; }
  if [[ $GCS_AUTH_OK -ne 1 ]]; then KIND=gcs-auth; return 1; fi

  ts="$(date -u +%Y%m%d-%H%M%S)"
  year="${ts:0:4}"; month="${ts:4:2}"
  out="$WORK/$name-$ts.sql.gz"
  OBJECT="gs://$bucket/$name/$year/$month/$name-$ts.sql.gz"

  # pg_dump reads its connection from the environment (never argv). The
  # subshell drops anything inherited that libpq would look at, so none of this
  # script's own variables may start with PG (they would be unset here too).
  local -a rcs
  local rawerr="$WORK/pg_dump.err"
  (
    for v in $(compgen -v PG); do unset "$v"; done
    for k in "${!CONNV[@]}"; do export "$k=${CONNV[$k]}"; done
    exec "$DUMP_BIN" -w
  ) 2>"$rawerr" | gzip -c >"$out"
  rcs=("${PIPESTATUS[@]}")
  scrub "$url" "${CONNV[PGPASSWORD]:-}" <"$rawerr" >>"$errf"
  local refused=0
  grep -q 'authentication method requirement' "$rawerr" 2>/dev/null && refused=1
  rm -f -- "$rawerr"
  if [[ "${rcs[0]}" -ne 0 || "${rcs[1]}" -ne 0 ]]; then
    KIND=dump-failed; [[ $refused -eq 0 ]] || KIND=auth-refused
    rm -f -- "$out"; return 1
  fi

  if ! "$VERIFY" "$out" >>"$errf" 2>&1; then KIND=verify-failed; rm -f -- "$out"; return 1; fi
  BYTES="$(stat -c %s -- "$out")"

  if ! gcloud storage cp -q -- "$out" "$OBJECT" >>"$errf" 2>&1; then
    KIND=upload-failed; rm -f -- "$out"; return 1
  fi
  rm -f -- "$out" "$errf"
  return 0
}

# ------------------------------------------------------------------ main

# Modes provision.sh runs as the backup user, before anything else is looked at.
if [[ "$MODE" == install-key ]]; then
  umask 0077
  [[ -d "$PRIV" && ! -L "$PRIV" ]] || { say "no private directory"; exit 1; }
  { [[ ! -e "$KEY_FILE" && ! -L "$KEY_FILE" ]] || [[ -f "$KEY_FILE" && ! -L "$KEY_FILE" ]]; } || { say "the key path is not a regular file"; exit 1; }
  ktmp="$(mktemp "$PRIV/.key.XXXXXX")" || exit 1
  if cat >"$ktmp" && [[ -s "$ktmp" ]] && chmod 0600 "$ktmp" && mv -f -- "$ktmp" "$KEY_FILE"; then
    say "key file written"; exit 0
  fi
  rm -f -- "$ktmp"; say "could not write the key file"; exit 1
fi
if [[ "$MODE" == mark-disabled ]]; then
  [[ -d "$STATUS_DIR" && -w "$STATUS_DIR" ]] || { say "status directory is not writable"; exit 1; }
  umask 0077
  status_edit '.enabled = false | .updated = $now' --arg now "$(now)" || { say "could not write the status file"; exit 1; }
  say "status marked disabled"; exit 0
fi

[[ -r "$CONFIG" ]] || { say "cannot read the config"; exit 1; }
jq -e '.databases | type == "array"' "$CONFIG" >/dev/null 2>&1 || { say "the config is not valid (no databases array)"; exit 1; }
[[ -d "$STATUS_DIR" && -w "$STATUS_DIR" ]] || { say "status directory is not writable"; exit 1; }
umask 0077

if [[ "$MODE" == init ]]; then
  init_status || { say "could not write the status file"; exit 1; }
  say "status file initialised"
  exit 0
fi

mkdir -p -- "$PRIV" "$WORK_ROOT" "$ERR_DIR" "$CLOUDSDK_CONFIG"

init_status || say "WARNING: could not refresh the status file"

# Work area for this run; leftovers of runs that were killed are swept.
find "$WORK_ROOT" -mindepth 1 -maxdepth 1 -name 'run-*' -mmin +1440 -exec rm -rf -- {} + 2>/dev/null || true
WORK="$(mktemp -d "$WORK_ROOT/run-XXXXXX")"
trap 'rm -rf -- "$WORK"' EXIT

load_connections
load_tunnel_ports
[[ $CONN_READABLE -eq 1 ]] || say "WARNING: the connections file is not readable; every database will fail with no-secret"

DUMP_BIN=""; DUMP_MAJOR=0
pick_pg_dump || say "WARNING: no pg_dump >= $DUMP_MIN_MAJOR found (best: ${DUMP_BIN:-none} major $DUMP_MAJOR)"
[[ $DUMP_MAJOR -ge $DUMP_MIN_MAJOR ]] || DUMP_BIN=""

GCS_AUTH_OK=0
if [[ -r "$KEY_FILE" ]] && gcloud auth activate-service-account --key-file="$KEY_FILE" -q >"$ERR_DIR/last-error-gcs-auth.log" 2>&1; then
  GCS_AUTH_OK=1; chmod 0600 "$ERR_DIR/last-error-gcs-auth.log"
else
  chmod 0600 "$ERR_DIR/last-error-gcs-auth.log" 2>/dev/null || true
  say "WARNING: could not authenticate to Google Cloud Storage with the key file; uploads will fail (gcs-auth)"
fi

say "start schedule=$SCHEDULE pg_dump_major=$DUMP_MAJOR"
SUCCEEDED=0
declare -a FAILED_NAMES=()
while IFS= read -r db_json; do
  name="$(jq -r '.name' <<<"$db_json")"
  secret="$(jq -r '.connection_secret' <<<"$db_json")"
  bucket="$(jq -r '.gcs_bucket' <<<"$db_json")"
  reqauth="$(jq -r 'if (has("require_auth") | not) or .require_auth == null then "" elif .require_auth == false then "@off" elif (.require_auth | type) == "string" then .require_auth else "@bad" end' <<<"$db_json")"
  t0=$SECONDS
  if backup_one "$name" "$secret" "$bucket" "$reqauth"; then
    SUCCEEDED=$((SUCCEEDED + 1))
    say "db=$name status=ok bytes=$BYTES"
    record_db "$name" "$SCHEDULE" true "" "$OBJECT" "$BYTES" $((SECONDS - t0)) || say "WARNING: could not update the status file"
  else
    FAILED_NAMES+=("$name")
    say "db=$name status=FAILED kind=$KIND"
    record_db "$name" "$SCHEDULE" false "$KIND" "" 0 $((SECONDS - t0)) || say "WARNING: could not update the status file"
  fi
done < <(jq -c --arg s "$SCHEDULE" '.databases[] | select(.enabled == true and .schedule == $s)' "$CONFIG")

failed_json="$(printf '%s\n' "${FAILED_NAMES[@]:-}" | jq -R . | jq -sc 'map(select(. != ""))')"
record_schedule "$SCHEDULE" "$SUCCEEDED" "$failed_json" || say "WARNING: could not update the status file"
say "done schedule=$SCHEDULE succeeded=$SUCCEEDED failed=${#FAILED_NAMES[@]}"
[[ ${#FAILED_NAMES[@]} -eq 0 ]]
