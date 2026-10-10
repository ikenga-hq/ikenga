#!/bin/bash
set -euo pipefail

# Provision a fresh Debian/Ubuntu host into a hardened, running ikenga-server.
#
#   provision.sh --profile <file> [--dry-run] [--yes] [--skip-hardening]
#   provision.sh --profile profiles/dixtrit-public.env --dry-run
#   provision.sh backups --profile <file> [--dry-run]    (Postgres backups to GCS only)
#   provision.sh tunnels --profile <file> [--dry-run]    (SSH tunnels to remote hosts only)
#   provision.sh swap --profile <file> [--dry-run]       (swap file + vm.swappiness only)
#
# One idempotent entry point. Every phase checks before it changes, and
# --dry-run prints what each phase WOULD change without touching the host.
# Secrets are never echoed: not to the terminal, not to the log, not in argv.
#
# Profiles are shell-sourced KEY=VALUE files (see profiles/). The perimeter is
# a profile field: `tailnet` binds the tailnet address only; `public-https`
# binds loopback and fronts it with Caddy + Let's Encrypt. Plan and rationale:
# plans/remote-provisioning/01-plan.md in the ikenga workspace repo.
#
# Supported AGENT_CLIS: claude (Claude Code), codex (OpenAI Codex CLI),
# opencode (OpenCode), pi (Pi Coding Agent), agy (Antigravity CLI).
# OpenRouter is a provider key, not a CLI: OPENROUTER_API_KEY (and other
# model provider keys) goes in the SECRETS_FROM file, never in the profile
# or argv. Among the installed CLIs, opencode and pi directly support
# OPENROUTER_API_KEY as a provider key.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${INSTALL_DIR:-/opt/ikenga}"
ENV_FILE="$INSTALL_DIR/.env"
REPO="ikenga-hq/ikenga"

DRY_RUN=0
ALLOW_DOWNGRADE=0
ASSUME_YES=0
SKIP_HARDENING=0
PROFILE_FILE=""
ACTION="provision"
TARGET_VERSION=""
UPGRADE_LATEST=0
FORCE=0
CHANNEL="stable"
HEALTH_TIMEOUT="${HEALTH_TIMEOUT:-20}"
RELEASE_BASE_URL="${RELEASE_BASE_URL:-https://github.com/$REPO/releases/download}"

# In-app updates (WP-P9 steps 2-3). Root owns all of this; the server only
# reads STATE_DIR and drops a request file. The server's side of the same
# contract is src-tauri/src/server/update.rs: keep the paths and schemas in
# step with it. The overrides exist for the container tests.
STATE_DIR="${IKENGA_UPDATE_STATE_DIR:-/var/lib/ikenga-update}"
STABLE_COPY="${IKENGA_PROVISION_STABLE:-/usr/local/sbin/ikenga-provision}"
SYSTEMD_DIR="${IKENGA_SYSTEMD_DIR:-/etc/systemd/system}"
# A request older than this is refused, so a file left over a reboot never
# fires a surprise restart.
UPDATE_REQUEST_MAX_AGE="${UPDATE_REQUEST_MAX_AGE:-900}"
# After a rolled-back or failed attempt at a version, refuse the same version
# for this long: a broken release must not become a restart loop that keeps
# ending people's terminals.
UPDATE_RETRY_COOLDOWN="${UPDATE_RETRY_COOLDOWN:-3600}"
SEMVER_RE='^[0-9]+\.[0-9]+\.[0-9]+$'

case "${1:-}" in
  upgrade|check-update|apply-request|install-update-units|install-agent-cli-updates|sync-accounts|backups|tunnels|swap) ACTION="$1"; shift ;;
esac

while [[ $# -gt 0 ]]; do
  case "$1" in
    upgrade) ACTION="upgrade"; shift ;;
    --to)
      TARGET_VERSION="${2:?--to needs a version}"
      [[ "$TARGET_VERSION" =~ $SEMVER_RE && ${#TARGET_VERSION} -le 32 ]] || { echo "error: --to must look like X.Y.Z" >&2; exit 2; }
      shift 2 ;;
    --latest) UPGRADE_LATEST=1; shift ;;
    --force) FORCE=1; shift ;;
    --profile) PROFILE_FILE="${2:?--profile needs a file}"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --allow-downgrade) ALLOW_DOWNGRADE=1; shift ;;
    --yes|-y) ASSUME_YES=1; shift ;;
    --skip-hardening) SKIP_HARDENING=1; shift ;;
    -h|--help)
      if [[ "$ACTION" == "upgrade" ]]; then
        printf 'Usage: %s upgrade [--profile <file>] [--to X.Y.Z|--latest] [--force] [--dry-run] [--yes]\n\n' "${BASH_SOURCE[0]}"
        printf 'Upgrade ikenga-server to a specified or latest version:\n'
        printf '  - reads release manifest for profile channel (stable)\n'
        printf '  - refuses if installed version is below min-upgrade-from\n'
        printf '  - verifies checksum and attestation\n'
        printf '  - keeps bin/ikenga-server.prev-<ver> for rollback\n'
        printf '  - swaps binary, restarts service, and health-checks /api/health\n'
        printf '  - automatically rolls back to previous binary if health-check fails\n'
        printf '  - refuses if open terminals are detected unless --force\n'
        printf '  - exit codes: 0 upgraded or already current, 3 rolled back, 4 rollback failed\n'
        exit 0
      fi
      if [[ "$ACTION" != "provision" ]]; then
        printf 'Usage: %s check-update | apply-request | install-update-units | install-agent-cli-updates | sync-accounts | backups | tunnels | swap [--profile <file>] [--dry-run]\n\n' "${BASH_SOURCE[0]}"
        printf '  check-update          read the release manifest and write %s/available.json (installs nothing)\n' "$STATE_DIR"
        printf '  apply-request         claim and apply an admin update request (run by ikenga-update.service)\n'
        printf '  install-update-units  install %s and the update timer, path and service units\n' "$STABLE_COPY"
        printf '  install-agent-cli-updates  install the daily npm update timer for the profile'"'"'s AGENT_CLIS\n'
        printf '  sync-accounts         converge shared project mirrors, per-account clones and scoped secrets\n'
        printf '                        (run it after creating or removing accounts; the full provision run does it too)\n'
        printf '  backups               converge the Postgres backup jobs (BACKUPS_ENABLED): backup user, pg_dump, gcloud, scoped\n'
        printf '                        secrets, systemd service and timers, status file (the full provision run does it too)\n'
        printf '  tunnels               converge the SSH tunnels (TUNNELS): tunnel user, one key, pinned known_hosts, one systemd unit\n'
        printf '                        per tunnel; prints the authorized_keys line to install on each remote host (runs before backups)\n'
        printf '  swap                  converge the swap file (SWAP_SIZE, SWAPPINESS, SWAP_FILE): one managed fstab line, vm.swappiness\n'
        printf '                        in a sysctl drop-in; does nothing when other swap is already active (the full provision run does it too)\n'
        exit 0
      fi
      sed -n '3,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
done

# A profile root may source unattended (the update units run as root with no
# arguments): a regular file owned by root that nobody else can write.
profile_trusted() {
  local f="$1" owner mode
  [[ -f "$f" && ! -L "$f" ]] || return 1
  read -r owner mode < <(stat -c '%u %a' -- "$f") || return 1
  [[ "$owner" == 0 ]] || return 1
  (( (8#$mode & 8#022) == 0 ))
}

if [[ "$ACTION" == "provision" ]]; then
  [[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" ]] || { echo "error: --profile <file> is required and must exist" >&2; exit 2; }
else
  if [[ -z "$PROFILE_FILE" && -f "$INSTALL_DIR/.profile.env" ]]; then
    if profile_trusted "$INSTALL_DIR/.profile.env"; then
      PROFILE_FILE="$INSTALL_DIR/.profile.env"
    else
      echo "WARNING: ignoring $INSTALL_DIR/.profile.env: not a root-owned file that only root can write" >&2
    fi
  elif [[ -n "$PROFILE_FILE" && "$ACTION" != "upgrade" ]] && ! profile_trusted "$PROFILE_FILE"; then
    echo "error: $PROFILE_FILE must be a root-owned file that only root can write" >&2; exit 2
  fi
fi

log()  { printf '==> %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }
CHANGES=()
changed() {
  if [[ $DRY_RUN -eq 1 ]]; then CHANGES+=("would: $*"); else CHANGES+=("$*"); fi
}

# Run a mutating command, or just print it under --dry-run. Reads are never
# wrapped: a dry run must still be able to look at the host.
run() {
  if [[ $DRY_RUN -eq 1 ]]; then printf '    [dry-run] %s\n' "$*"; else "$@"; fi
}

# ---------------------------------------------------------------- profile

# Defaults first, so a profile only states what differs.
HOSTNAME_WANT=""
TIER="t1"
PERIMETER="tailnet"
VERSION=""
ADMIN_USER=""
ADMIN_SSH_KEYS_FILE=""
PUBLIC_HOST=""
ACME_EMAIL=""
TS_AUTHKEY_FILE=""
TS_HOSTNAME=""
SSH_PORT="22"
SSH_ACCESS=""          # public | tailnet (default: public for public-https)
APPS=()
LIBS=()
AGENT_CLIS=()
FS_ROOTS=()
SECRETS_FROM=""
# Shared project clones and scoped secrets (D-B2, D-B3). See README "Shared
# projects and scoped secrets". Accounts are created by `ikenga-server accounts
# create`, not by this script; these keys only name them.
ACCOUNTS=()            # login names (unix user = ik-<name>); empty = every ik-* user in UID_RANGE
AGENT_ACCOUNTS=()      # the accounts a secret scoped `agents` goes to (also managed)
UID_RANGE="20000-29999"
PROJECTS_DIR="/srv/ikenga/projects"
PROJECTS_READ="members"     # who can read the root-owned mirrors: members (read-only ACLs) | world
PROJECTS_MEMBERS=()    # empty = every managed account
PROJECTS=()            # name=git-url[#branch]
PROJECTS_BRANCH_PREFIX=""   # each account's branch = <prefix><account>/main
PROJECTS_TOKEN_SECRET=""    # NAME of a SECRETS_FILE entry: https deploy token for private repos
SECRETS_FILE=""             # root-only scoped secrets (format in README)
# Postgres backups to GCS as system jobs (D-B10). See README "Database backups".
BACKUPS_ENABLED=0
BACKUP_USER="ikenga-backup"     # a plain system user, NOT an Ikenga principal
BACKUP_CONFIG=""                # path to a JSON file: {"databases":[{name,connection_secret,schedule,gcs_bucket,enabled}]}
BACKUP_GCS_KEY_SECRET=""        # NAME of a SECRETS_FILE entry: the service-account key JSON, base64 on one line
BACKUP_SCHEDULES=()             # name=OnCalendar overrides/additions (defaults: 4hourly daily weekly, UTC)
BACKUP_PG_MAJOR="17"            # postgresql-client-<n> from the PGDG apt repo
BACKUP_TIMEOUT_SEC="10800"      # a run is killed after this (keep it under the shortest interval)
BACKUP_GCLOUD_KEY_FPRS=()       # extra accepted fingerprints for the Google Cloud apt signing key

# SSH tunnels (e.g. to a database the backups read). See README "SSH tunnels".
# TUNNELS is deliberately NOT given a default here: a profile that sets it (even
# to the empty list) hands this script ownership of every tunnel unit of
# TUNNEL_USER, a profile that never mentions it leaves the host's tunnels alone.
TUNNEL_USER="ikenga-tunnel"     # a plain system user, NOT an Ikenga principal; owns the one key
TUNNEL_FROM=""                  # the address printed in the authorized_keys from="" option (default: this box's public address)
TUNNEL_KNOWN_HOSTS=()           # "<host> <keytype> <base64>": the pinned host keys; host is "name" or "[name]:port"
# TUNNEL_ALLOW_USERS: who may connect to a tunnel's local port (root always may).
# Like TUNNELS it is left UNSET here on purpose: unset = the backup user when
# BACKUPS_ENABLED=1, else nobody but root; ( ) = root only; (a b) = root, a and b.
unset TUNNELS TUNNEL_ALLOW_USERS

# Swap (README "Swap"). A small box with several agent sessions needs some, or
# the OOM killer ends a session. auto = 4G for RAM up to 8 GB, 2G above;
# off|0 = manage nothing and remove what a previous run made.
SWAP_SIZE="auto"                # auto | <n>M | <n>G | off | 0
SWAPPINESS="10"                 # vm.swappiness (0-100): use swap as an overflow, not eagerly
SWAP_FILE="/swapfile"           # an absolute path on a local ext4/xfs/f2fs filesystem

if [[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$PROFILE_FILE"
elif [[ "$ACTION" != "provision" ]]; then
  # No profile on this box. The tier is whichever unit is installed. The .env
  # is NEVER sourced: it is a secrets file, and it holds none of these
  # settings anyway (an earlier fallback executed it as shell, as root).
  if [[ -f "$SYSTEMD_DIR/ikenga-server-t1.service" ]]; then TIER=t1
  elif [[ -f "$SYSTEMD_DIR/ikenga-server.service" ]]; then TIER=t0
  fi
fi
TUNNEL_ALLOW_DEFINED=0
if declare -p TUNNEL_ALLOW_USERS >/dev/null 2>&1; then
  [[ "$(declare -p TUNNEL_ALLOW_USERS)" == "declare -a"* ]] || { echo "error: TUNNEL_ALLOW_USERS must be a bash array: TUNNEL_ALLOW_USERS=( ikenga-backup )" >&2; exit 2; }
  TUNNEL_ALLOW_DEFINED=1
else
  TUNNEL_ALLOW_USERS=()
fi
TUNNELS_DEFINED=0
if declare -p TUNNELS >/dev/null 2>&1; then
  [[ "$(declare -p TUNNELS)" == "declare -a"* ]] || { echo "error: TUNNELS must be a bash array: TUNNELS=( \"name=user@host 5544:127.0.0.1:5432\" )" >&2; exit 2; }
  TUNNELS_DEFINED=1
else
  TUNNELS=()
fi

validate_profile() {
  local _cli
  for _cli in "${AGENT_CLIS[@]}"; do
    case "$_cli" in claude|codex|opencode|pi|agy) ;; *) die "unknown AGENT_CLI '$_cli' (allowed: claude codex opencode pi agy)" ;; esac
  done
  [[ "$TIER" == t0 || "$TIER" == t1 ]] || die "TIER must be t0 or t1 (got '$TIER')"
  [[ "$PERIMETER" == tailnet || "$PERIMETER" == public-https ]] || die "PERIMETER must be tailnet or public-https (got '$PERIMETER')"
  [[ -n "$VERSION" ]] || die "VERSION is required (e.g. VERSION=0.18.2); the provisioner never installs 'latest' silently"
  [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "VERSION must look like X.Y.Z (got '$VERSION')"
  [[ -n "$ADMIN_USER" ]] || die "ADMIN_USER is required (the sudo user you will SSH in as once root login is closed)"
  [[ "$ADMIN_USER" =~ ^[a-z][a-z0-9_-]{0,30}$ ]] || die "ADMIN_USER '$ADMIN_USER' is not a valid unix user name"
  [[ -n "$ADMIN_SSH_KEYS_FILE" && -f "$ADMIN_SSH_KEYS_FILE" ]] || die "ADMIN_SSH_KEYS_FILE must point to a file of public keys (locking SSH to keys with no key installed locks you out)"
  if [[ "$PERIMETER" == public-https ]]; then
    # The contract (G-PRINCIPAL) accepts public exposure of T0 only for one
    # trusted operator with the bearer token as the sole gate. Refuse it.
    [[ "$TIER" == t1 ]] || die "PERIMETER=public-https requires TIER=t1 (a single shared bearer token must not be the only gate on a public host)"
    [[ -n "$PUBLIC_HOST" ]] || die "PERIMETER=public-https requires PUBLIC_HOST"
    SSH_ACCESS="${SSH_ACCESS:-public}"
    [[ "$SSH_ACCESS" == public || "$SSH_ACCESS" == tailnet ]] || die "SSH_ACCESS must be public or tailnet (got '$SSH_ACCESS')"
  else
    SSH_ACCESS=tailnet
  fi
  # The auth key is only needed to JOIN; a host already on the tailnet
  # (re-run, or a perimeter switch) does not need one. tailnet_join enforces it.
  [[ -z "$TS_AUTHKEY_FILE" || -f "$TS_AUTHKEY_FILE" ]] || die "TS_AUTHKEY_FILE '$TS_AUTHKEY_FILE' does not exist"
  [[ -z "$SECRETS_FROM" || -f "$SECRETS_FROM" ]] || die "SECRETS_FROM '$SECRETS_FROM' does not exist"
  validate_accounts_profile
  validate_tunnels_profile
  validate_backups_profile
  validate_swap_profile
}

# --------------------------------------------------------------- preflight

detect_arch() {
  case "$(uname -m)" in
    x86_64) ARCH=amd64 ;;
    aarch64|arm64) ARCH=arm64; note "arm64: newer and less tested than amd64 (README)" ;;
    *) die "unsupported architecture $(uname -m)" ;;
  esac
}

preflight() {
  log "Preflight"
  [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo); dry runs may be unprivileged"
  [[ "$(uname -s)" == Linux ]] || die "Linux only"
  if ! command -v systemctl >/dev/null; then
    [[ $DRY_RUN -eq 1 ]] && note "WARNING: systemd not found (tolerated in --dry-run only)" || die "systemd is required"
  fi

  detect_arch

  # Read os-release in a subshell: sourcing it into this shell would clobber
  # profile variables (it defines VERSION, ID, NAME, ...).
  local os_id os_pretty
  os_id="$(. /etc/os-release && printf '%s' "${ID:-}")"
  os_pretty="$(. /etc/os-release && printf '%s' "${PRETTY_NAME:-$os_id}")"
  case "$os_id" in
    ubuntu|debian) ;;
    *) die "only Debian and Ubuntu are supported (got ${os_id:-unknown})" ;;
  esac

  local glibc; glibc="$(ldd --version 2>&1 | head -1 | grep -oE '[0-9]+\.[0-9]+$' || true)"
  if [[ -n "$glibc" ]] && awk "BEGIN{exit !($glibc < 2.31)}"; then
    die "glibc $glibc is older than the 2.31 floor the release is built against"
  fi

  local free_kb; free_kb="$(df -Pk / | awk 'NR==2{print $4}')"
  (( free_kb > 2*1024*1024 )) || die "less than 2 GB free on /"

  # A listener we did not start would either fail the bind or, worse, be
  # fronted by us. Skip the check when our own service already owns the port.
  if ss -ltn 2>/dev/null | grep -qE ':4000\s' && ! systemctl is-active --quiet "ikenga-server*" 2>/dev/null; then
    die "port 4000 is already in use by something other than ikenga-server"
  fi

  if [[ "$PERIMETER" == public-https ]] && ss -ltn 2>/dev/null | grep -qE ':(80|443)\s' && ! systemctl is-active --quiet caddy 2>/dev/null; then
    die "port 80/443 is already in use; Caddy needs both for ACME"
  fi
  note "os=$os_pretty arch=$ARCH tier=$TIER perimeter=$PERIMETER version=$VERSION"
}

confirm() {
  [[ $ASSUME_YES -eq 1 || $DRY_RUN -eq 1 ]] && return 0
  printf 'Proceed with provisioning this host as %s / %s? [y/N] ' "$TIER" "$PERIMETER"
  read -r reply
  [[ "$reply" == y || "$reply" == Y ]] || die "aborted"
}

# -------------------------------------------------------------- hardening

apt_install() {
  local missing=()
  for p in "$@"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
  if [[ ${#missing[@]} -gt 0 ]]; then
    run env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "${missing[@]}"
    changed "apt packages installed: ${missing[*]}"
  fi
}

harden_base() {
  log "Base packages and hardening"
  run env DEBIAN_FRONTEND=noninteractive apt-get update -y
  apt_install ca-certificates curl git tmux procps openssl gnupg unzip sudo ufw fail2ban unattended-upgrades

  if [[ -n "$HOSTNAME_WANT" && "$(hostname)" != "$HOSTNAME_WANT" ]]; then
    run hostnamectl set-hostname "$HOSTNAME_WANT"; changed "hostname -> $HOSTNAME_WANT"
  fi

  if [[ $SKIP_HARDENING -eq 1 ]]; then note "--skip-hardening: leaving SSH and firewall alone"; return; fi

  # Admin user first, key installed BEFORE root login or passwords are closed.
  if ! id "$ADMIN_USER" >/dev/null 2>&1; then
    run adduser --disabled-password --gecos "" "$ADMIN_USER"; changed "user $ADMIN_USER created"
  fi
  run usermod -aG sudo "$ADMIN_USER"
  run install -d -m 0700 -o "$ADMIN_USER" -g "$ADMIN_USER" "/home/$ADMIN_USER/.ssh"
  if ! cmp -s "$ADMIN_SSH_KEYS_FILE" "/home/$ADMIN_USER/.ssh/authorized_keys" 2>/dev/null; then
    run install -m 0600 -o "$ADMIN_USER" -g "$ADMIN_USER" "$ADMIN_SSH_KEYS_FILE" "/home/$ADMIN_USER/.ssh/authorized_keys"
    changed "authorized_keys for $ADMIN_USER installed"
  fi
  # Key-only sudo is fine for a box with no password; require it explicitly.
  if [[ ! -f "/etc/sudoers.d/90-$ADMIN_USER" ]]; then
    run sh -c "printf '%s ALL=(ALL) NOPASSWD:ALL\n' '$ADMIN_USER' > '/etc/sudoers.d/90-$ADMIN_USER' && chmod 440 '/etc/sudoers.d/90-$ADMIN_USER'"
    changed "sudoers drop-in for $ADMIN_USER"
  fi

  # SSH lockdown happens in its own drop-in, and only after the key is in
  # place and sshd accepts the new config. `sshd -t` is the guard: a bad file
  # never reloads.
  local dropin=/etc/ssh/sshd_config.d/90-ikenga-hardening.conf
  local want; want="PasswordAuthentication no
PermitRootLogin prohibit-password
KbdInteractiveAuthentication no
Port $SSH_PORT
"
  if [[ "$(cat "$dropin" 2>/dev/null || true)" != "${want%$'\n'}" ]]; then
    if [[ $DRY_RUN -eq 1 ]]; then
      note "[dry-run] write $dropin (key-only SSH), validate with sshd -t, reload ssh"
    else
      printf '%s' "$want" > "$dropin"
      if sshd -t; then systemctl reload ssh 2>/dev/null || systemctl reload sshd; changed "SSH locked to keys (drop-in $dropin)"
      else rm -f "$dropin"; die "sshd rejected the hardening drop-in; removed it, nothing changed"; fi
    fi
  fi

  run systemctl enable --now fail2ban
  run systemctl enable --now unattended-upgrades
}

# ---------------------------------------------------------------- firewall

# Applied AFTER the perimeter so the rule set matches what is actually
# listening, and after SSH is confirmed on its port.
firewall() {
  [[ $SKIP_HARDENING -eq 1 ]] && return
  log "Firewall (ufw): default deny inbound, exactly this profile's rules"

  # The profile is the whole rule set. Every rule we add carries an
  # `ikenga:` comment; if the live set differs from the wanted set in any way
  # (a rule from a previous perimeter, a hand-added rule, a missing one) the
  # table is reset and rebuilt. Without this, switching a host from
  # public-https to tailnet would leave 22/80/443 open on the public address.
  local -a rules
  if [[ "$PERIMETER" == public-https ]]; then
    local ssh_rule="limit $SSH_PORT/tcp comment ikenga:ssh-public-ratelimited"
    [[ "$SSH_ACCESS" == tailnet ]] && ssh_rule="allow in on tailscale0 to any port $SSH_PORT proto tcp comment ikenga:ssh-tailnet"
    rules=(
      "$ssh_rule"
      "allow 80/tcp comment ikenga:acme-http01"
      "allow 443/tcp comment ikenga:https"
    )
  else
    # Tailnet: nothing on the public interface. SSH and the daemon only over
    # tailscale0. perimeter_tailnet has already proven tailscale0 is up.
    rules=(
      "allow in on tailscale0 to any port $SSH_PORT proto tcp comment ikenga:ssh-tailnet"
      "allow in on tailscale0 to any port 4000 proto tcp comment ikenga:daemon-tailnet"
    )
  fi

  local want have
  want="$(printf '%s\n' "${rules[@]}" | grep -oE 'ikenga:[a-z0-9-]+' | sort)"
  # `|| true`: on a fresh host there are no rules, grep exits 1, and under
  # pipefail + errexit that silently killed the whole run.
  have="$( { ufw show added 2>/dev/null | grep -E '^ufw ' || true; } | while read -r line; do
            c="$(grep -oE 'ikenga:[a-z0-9-]+' <<<"$line" || true)"; echo "${c:-foreign}"; done | sort)"
  local active=0; ufw status 2>/dev/null | grep -q '^Status: active' && active=1

  if [[ "$want" == "$have" && $active -eq 1 ]]; then
    note "ufw already matches the $PERIMETER rule set"
    return
  fi

  run ufw --force reset >/dev/null
  run ufw default deny incoming
  run ufw default allow outgoing
  local r
  # shellcheck disable=SC2086
  for r in "${rules[@]}"; do run ufw $r; done
  run ufw --force enable
  [[ "$PERIMETER" == tailnet ]] && note "SSH on the public interface is now CLOSED. Confirm tailnet SSH works from a second session before you leave this one."
  changed "ufw rebuilt to the $PERIMETER rule set (${#rules[@]} rules)"
}

# --------------------------------------------------------------- perimeter

# Join the tailnet (idempotent). Used by the tailnet perimeter, and by
# public-https when SSH_ACCESS=tailnet keeps SSH off the public interface.
tailnet_join() {
  if ! command -v tailscale >/dev/null; then
    run sh -c 'curl -fsSL https://tailscale.com/install.sh | sh'
    changed "tailscale installed"
  fi
  if ! tailscale status >/dev/null 2>&1; then
    # `file:` makes tailscale read the key itself, so it never appears in argv
    # (`ps`, /proc/*/cmdline) or this script's environment. An earlier
    # `TS_AUTHKEY=$(cat f) tailscale up --auth-key="$TS_AUTHKEY"` passed an
    # EMPTY key (a prefix assignment is not visible to the same command's
    # argument expansion), and `tailscale up` then waited forever for an
    # interactive browser login. --timeout makes a bad key fail, not hang.
    [[ -n "$TS_AUTHKEY_FILE" ]] || die "not on a tailnet and no TS_AUTHKEY_FILE to join with"
    run tailscale up --auth-key="file:$TS_AUTHKEY_FILE" ${TS_HOSTNAME:+--hostname="$TS_HOSTNAME"} --ssh=false --timeout=90s \
      || die "tailscale up failed (bad, expired or already-used auth key, or the key's tags are not allowed by the tailnet policy)"
    changed "joined tailnet"
  fi
  if [[ $DRY_RUN -eq 0 ]]; then
    TS_IP="$(tailscale ip -4 | head -1)"
    [[ -n "$TS_IP" ]] || die "no tailnet address after 'tailscale up'"
  else
    TS_IP="<tailnet-ip>"
  fi
}

perimeter_tailnet() {
  log "Perimeter: tailnet"
  tailnet_join
  IKENGA_HOST_VALUE="$TS_IP"
  # A host moving from public-https keeps Caddy listening on 80/443 unless we
  # retire it; the firewall closes the ports, but nothing should be serving.
  if systemctl is-enabled --quiet caddy 2>/dev/null || systemctl is-active --quiet caddy 2>/dev/null; then
    run systemctl disable --now caddy
    changed "caddy disabled (tailnet perimeter has no public listener)"
  fi
  note "Operator action: add an ACL rule letting your users reach this host on tcp:4000 (default-deny tailnet)."
}

perimeter_public() {
  log "Perimeter: public-https (Caddy + Let's Encrypt)${SSH_ACCESS:+, SSH over $SSH_ACCESS}"
  [[ "$SSH_ACCESS" == tailnet ]] && tailnet_join
  if ! command -v caddy >/dev/null; then
    run sh -c 'curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/gpg.key | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg'
    run sh -c "curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt -o /etc/apt/sources.list.d/caddy-stable.list"
    run env DEBIAN_FRONTEND=noninteractive apt-get update -y
    apt_install caddy
  fi
  # Request body cap and security headers. The daemon has no trusted-proxy
  # setting yet (WP-P4): it sees 127.0.0.1 for every client, so per-IP rate
  # limits belong here, not in the daemon, until that lands.
  local conf=""
  [[ -n "$ACME_EMAIL" ]] && conf="{
	email $ACME_EMAIL
}

"
  conf+="$PUBLIC_HOST {
	encode zstd gzip
	request_body {
		max_size 12MB
	}
	header {
		Strict-Transport-Security \"max-age=31536000; includeSubDomains\"
		X-Content-Type-Options nosniff
		Referrer-Policy no-referrer
		-Server
	}
	reverse_proxy 127.0.0.1:4000
}
"
  if [[ "$(cat /etc/caddy/Caddyfile 2>/dev/null || true)" != "${conf%$'\n'}" ]]; then
    if [[ $DRY_RUN -eq 1 ]]; then note "[dry-run] write /etc/caddy/Caddyfile for $PUBLIC_HOST, validate, reload"
    else
      cp -a /etc/caddy/Caddyfile "/etc/caddy/Caddyfile.bak-$(date +%Y%m%d-%H%M%S)" 2>/dev/null || true
      printf '%s' "$conf" > /etc/caddy/Caddyfile
      caddy validate --config /etc/caddy/Caddyfile >/dev/null || die "Caddyfile did not validate; backup left beside it"
      systemctl enable --now caddy; systemctl reload caddy
      changed "Caddy configured for $PUBLIC_HOST"
    fi
  fi
  # A host moving here from the tailnet perimeter had Caddy disabled; the
  # Caddyfile may be unchanged, so enabling cannot hang off the write above.
  if [[ $DRY_RUN -eq 0 ]] && ! systemctl is-active --quiet caddy; then
    systemctl enable --now caddy; changed "caddy enabled and started"
  fi
  IKENGA_HOST_VALUE="127.0.0.1"
  IKENGA_PUBLIC_URL_VALUE="https://$PUBLIC_HOST"
  note "DNS: $PUBLIC_HOST must resolve to this host before Caddy can obtain a certificate."
}

# ------------------------------------------------------- host dependencies

install_deps() {
  log "Host dependencies and libraries"
  if ! command -v bun >/dev/null && [[ ! -x /usr/local/bin/bun ]]; then
    run sh -c 'curl -fsSL https://bun.sh/install | BUN_INSTALL=/usr/local bash'
    changed "bun installed"
  fi
  # Node 22: better-sqlite3 is ABI-bound (a clean v20 crash-loops the sidecar).
  if ! node -v 2>/dev/null | grep -q '^v22\.'; then
    run sh -c 'curl -fsSL https://deb.nodesource.com/setup_22.x | bash -'
    apt_install nodejs
  fi
  local lib
  for lib in "${LIBS[@]}"; do
    case "$lib" in
      bun|node22) ;;                       # handled above
      ffmpeg|git-lfs|jq|ripgrep|sqlite3|build-essential|python3|python3-pip) apt_install "$lib" ;;
      *) die "unknown LIB '$lib' (allowed: bun node22 ffmpeg git-lfs jq ripgrep sqlite3 build-essential python3 python3-pip)" ;;
    esac
  done
  local cli
  for cli in "${AGENT_CLIS[@]}"; do
    case "$cli" in
      claude)
        command -v claude >/dev/null || { run npm install -g @anthropic-ai/claude-code; changed "claude CLI installed"; } ;;
      codex)
        command -v codex >/dev/null || { run npm install -g @openai/codex; changed "codex CLI installed"; } ;;
      opencode)
        command -v opencode >/dev/null || { run npm install -g opencode-ai; changed "opencode CLI installed"; } ;;
      pi)
        command -v pi >/dev/null || { run npm install -g @earendil-works/pi-coding-agent; changed "pi CLI installed"; } ;;
      agy)
        command -v agy >/dev/null || {
          # Official installer auto-detects architecture (amd64 vs arm64) and installs system-wide to /usr/local/bin
          run sh -c 'curl -fsSL https://antigravity.google/cli/install.sh | bash -s -- --dir /usr/local/bin'
          run chmod 0755 /usr/local/bin/agy
          changed "agy CLI installed"
        } ;;
      *) die "unknown AGENT_CLI '$cli' (allowed: claude codex opencode pi agy)" ;;
    esac
  done
}

# ------------------------------------------------------------ the daemon

install_daemon() {
  log "Install ikenga-server $VERSION"
  local have=""; have="$("$INSTALL_DIR/bin/ikenga-server" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || true)"
  if [[ "$have" == "$VERSION" ]]; then note "already $VERSION"; return; fi
  # A profile that still names an older release must never silently replace a
  # newer install: the tarball would overwrite bin/, dist/ and the units, and
  # the next restart would run old code on data a newer release migrated. This
  # happened on 2026-10-06 (VERSION=0.19.4 left in a profile after an upgrade
  # to 0.20.1). Downgrades need an explicit --allow-downgrade.
  if [[ -n "$have" ]] && semver_lt "$VERSION" "$have" && [[ $ALLOW_DOWNGRADE -ne 1 ]]; then
    die "installed ikenga-server $have is newer than the profile's VERSION=$VERSION; refusing to downgrade. Set VERSION=$have in the profile (or pass --allow-downgrade if you really mean it)."
  fi

  local tmp; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  local tarball="ikenga-server_${VERSION}_linux_${ARCH}.tar.gz"
  local base="https://github.com/$REPO/releases/download/v$VERSION"
  if [[ $DRY_RUN -eq 1 ]]; then
    note "[dry-run] download $base/$tarball + SHA256SUMS.txt, verify, unpack to $INSTALL_DIR"
    return
  fi
  curl -fsSL -o "$tmp/$tarball" "$base/$tarball"
  curl -fsSL -o "$tmp/SHA256SUMS.txt" "$base/SHA256SUMS.txt"
  (cd "$tmp" && sha256sum -c --ignore-missing SHA256SUMS.txt 2>&1 | grep -q "$tarball: OK") || die "checksum mismatch for $tarball; nothing installed"
  if command -v gh >/dev/null; then
    gh attestation verify "$tmp/$tarball" -R "$REPO" >/dev/null 2>&1 || note "WARNING: attestation not verified (gh not authenticated or failed); checksum matched"
  else
    note "gh not installed: attestation not checked (checksum matched)"
  fi

  install -d -m 0755 "$INSTALL_DIR"
  if [[ -x "$INSTALL_DIR/bin/ikenga-server" && -n "$have" ]]; then
    cp -a "$INSTALL_DIR/bin/ikenga-server" "$INSTALL_DIR/bin/ikenga-server.prev-$have"
    note "previous binary kept as ikenga-server.prev-$have (rollback)"
  fi
  tar -xzf "$tmp/$tarball" -C "$INSTALL_DIR"
  [[ "$("$INSTALL_DIR/bin/ikenga-server" --version | grep -oE '[0-9]+\.[0-9]+\.[0-9]+')" == "$VERSION" ]] || die "installed binary does not report $VERSION"
  install -d -m 0700 "$INSTALL_DIR/data"
  BINARY_CHANGED=1
  changed "ikenga-server $VERSION installed to $INSTALL_DIR"
}

# Append KEY=VALUE only if KEY is absent: idempotent, and never overwrites a
# credential that exists nowhere else. Values are never printed.
ensure_var() {
  local key="$1" value="$2"
  grep -qE "^${key}=" "$ENV_FILE" 2>/dev/null && { note "$key: already set, left as-is"; return; }
  printf '%s=%s\n' "$key" "$value" >> "$ENV_FILE"
  changed "$key added to $ENV_FILE"
}

# Perimeter-derived settings are not secrets and must follow the profile:
# set_var replaces a stale value, unset_var removes one. (ensure_var stays
# append-only for secrets, which may exist nowhere else.)
set_var() {
  local key="$1" value="$2" cur
  cur="$(grep -E "^${key}=" "$ENV_FILE" 2>/dev/null | head -1 | cut -d= -f2- || true)"
  if grep -qE "^${key}=" "$ENV_FILE" 2>/dev/null; then
    [[ "$cur" == "$value" ]] && return
    awk -v k="$key" -v v="$value" 'BEGIN{FS=OFS="="} $1==k{print k"="v; next} {print}' "$ENV_FILE" > "$ENV_FILE.tmp" \
      && cat "$ENV_FILE.tmp" > "$ENV_FILE" && rm -f "$ENV_FILE.tmp"
    changed "$key updated in $ENV_FILE"
  else
    printf '%s=%s\n' "$key" "$value" >> "$ENV_FILE"
    changed "$key added to $ENV_FILE"
  fi
  ENV_CHANGED=1
}
unset_var() {
  local key="$1"
  grep -qE "^${key}=" "$ENV_FILE" 2>/dev/null || return 0
  grep -vE "^${key}=" "$ENV_FILE" > "$ENV_FILE.tmp" && cat "$ENV_FILE.tmp" > "$ENV_FILE" && rm -f "$ENV_FILE.tmp"
  changed "$key removed from $ENV_FILE"
  ENV_CHANGED=1
}

write_env() {
  log "Credentials and environment ($ENV_FILE)"
  if [[ $DRY_RUN -eq 1 ]]; then
    note "[dry-run] ensure $ENV_FILE (root:root 600), IKENGA_HOST, IKENGA_PUBLIC_URL, vault key${TIER:+, token if t0}"
    return
  fi
  install -d -m 0755 "$INSTALL_DIR"
  [[ -f "$ENV_FILE" ]] && cp -a "$ENV_FILE" "$ENV_FILE.bak-$(date +%Y%m%d-%H%M%S)"
  touch "$ENV_FILE"; chmod 600 "$ENV_FILE"; chown root:root "$ENV_FILE"

  set_var IKENGA_HOST "$IKENGA_HOST_VALUE"
  if [[ -n "${IKENGA_PUBLIC_URL_VALUE:-}" ]]; then set_var IKENGA_PUBLIC_URL "$IKENGA_PUBLIC_URL_VALUE"; else unset_var IKENGA_PUBLIC_URL; fi
  ensure_var IKENGA_VAULT_KEY "$(openssl rand -hex 32)"
  [[ "$TIER" == t0 ]] && ensure_var IKENGA_AUTH_TOKEN "$(openssl rand -hex 32)"
  # Plain HTTP on a tailnet: the session cookie cannot be Secure (README).
  # Behind HTTPS it MUST be Secure, so a host leaving the tailnet drops it.
  if [[ "$TIER" == t1 && "$PERIMETER" == tailnet ]]; then set_var IKENGA_INSECURE_COOKIE true; else unset_var IKENGA_INSECURE_COOKIE; fi
  # Behind Caddy on loopback every client arrives from 127.0.0.1; trusting only
  # that peer lets the daemon read Caddy's X-Forwarded-For (which Caddy
  # overwrites, so clients cannot spoof it). The tailnet perimeter has no proxy.
  if [[ "$PERIMETER" == public-https ]]; then set_var IKENGA_TRUSTED_PROXIES 127.0.0.1; else unset_var IKENGA_TRUSTED_PROXIES; fi

  # Optional secrets (agent API keys etc.) from a file the operator controls:
  # copied by name, never read into argv or the log.
  if [[ -n "$SECRETS_FROM" ]]; then
    local k v
    while IFS='=' read -r k v; do
      [[ "$k" =~ ^[A-Z][A-Z0-9_]*$ ]] || continue
      ensure_var "$k" "$v"
    done < "$SECRETS_FROM"
  fi
}

install_service() {
  log "Service ($TIER)"
  local unit="ikenga-server.service"; [[ "$TIER" == t1 ]] && unit="ikenga-server-t1.service"
  local svc="${unit%.service}"

  if [[ "$TIER" == t0 ]]; then
    # T0 runs as a dedicated unprivileged service user, not as an operator.
    id ikenga >/dev/null 2>&1 || { run adduser --system --group --home /home/ikenga --shell /bin/bash ikenga; changed "service user ikenga"; }
    run chown -R ikenga:ikenga "$INSTALL_DIR/data"
  fi

  if [[ $DRY_RUN -eq 1 ]]; then note "[dry-run] install $unit, probe, enable --now $svc"; return; fi

  [[ -f "$INSTALL_DIR/$unit" ]] || die "$INSTALL_DIR/$unit missing from the tarball"
  # The T0 unit names User=rex in historical builds; rewrite to the service user.
  sed -e 's/^User=.*/User=ikenga/' -e 's/^Group=.*/Group=ikenga/' -e 's#/home/rex#/home/ikenga#g' "$INSTALL_DIR/$unit" > "/etc/systemd/system/$unit" || true
  [[ "$TIER" == t1 ]] && install -m 0644 "$INSTALL_DIR/$unit" "/etc/systemd/system/$unit"
  chmod 0644 "/etc/systemd/system/$unit"

  if [[ "$TIER" == t1 ]]; then
    "$INSTALL_DIR/bin/ikenga-server" probe --executor-tier t1 --data-dir "$INSTALL_DIR/data" \
      || die "T1 probe failed: this host cannot run multi-user mode. The server never falls back to a weaker tier; fix the host or use TIER=t0."
    if ! "$INSTALL_DIR/bin/ikenga-server" accounts --data-dir "$INSTALL_DIR/data" list 2>/dev/null | grep -q .; then
      note "No accounts yet. Create the first admin (prompts for a password):"
      note "  sudo $INSTALL_DIR/bin/ikenga-server accounts --data-dir $INSTALL_DIR/data create <name> --admin"
      note "The service is enabled but sign-in needs that admin."
    fi
  fi
  systemctl daemon-reload
  local was_active=0
  systemctl is-active --quiet "$svc" && was_active=1
  systemctl enable --now "$svc"
  # `enable --now` leaves an already-running service on the OLD binary, so an
  # upgrade (bumped VERSION) must restart it explicitly. This ends open
  # terminals (T1 keeps detached chi-runners via KillMode=process).
  if [[ ( $BINARY_CHANGED -eq 1 || $ENV_CHANGED -eq 1 ) && $was_active -eq 1 ]]; then
    systemctl restart "$svc"
    changed "service $svc restarted (new binary or settings; open terminals ended)"
  elif [[ $was_active -eq 0 ]]; then
    changed "service $svc enabled and started"
  fi
}

# ---------------------------------------------------------------- verify

verify() {
  log "Verify"
  if [[ $DRY_RUN -eq 1 ]]; then note "[dry-run] /api/health, tier, unauthenticated RPC = 401, perimeter reachability"; return; fi
  local i url="http://127.0.0.1:4000"
  [[ "$PERIMETER" == tailnet ]] && url="http://$IKENGA_HOST_VALUE:4000"
  for i in $(seq 1 20); do curl -fsS "$url/api/health" >/dev/null 2>&1 && break; sleep 1; done
  curl -fsS "$url/api/health" >/dev/null || die "/api/health did not answer at $url; see: journalctl -u ikenga-server* -n 50"
  note "health OK at $url"

  local code; code="$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'content-type: application/json' -d '{}' "$url/api/rpc" || true)"
  [[ "$code" == 401 || "$code" == 403 ]] && note "unauthenticated RPC refused ($code)" || die "unauthenticated RPC returned $code, expected 401/403"

  if [[ "$PERIMETER" == tailnet ]]; then
    local pub; pub="$(ip -4 route get 1.1.1.1 2>/dev/null | grep -oE 'src [0-9.]+' | awk '{print $2}')"
    if [[ -n "$pub" ]] && curl -s -m3 -o /dev/null "http://$pub:4000/api/health"; then die "daemon answers on the public address $pub:4000; perimeter is broken"; fi
    note "public address does not serve the daemon"
  fi
  if [[ "$PERIMETER" == public-https ]]; then
    ss -ltn | grep -qE '127\.0\.0\.1:4000\s' && note "daemon bound to loopback only" || die "daemon is not bound to 127.0.0.1:4000"
    note "From OUTSIDE this host, confirm: curl -I https://$PUBLIC_HOST/ (valid cert) and that port 4000 is closed."
  fi
}

summary() {
  log "Summary"
  if [[ ${#CHANGES[@]} -eq 0 ]]; then note "no changes: the host already matches this profile"; return; fi
  printf '    - %s\n' "${CHANGES[@]}"
  if [[ "$ACTION" == sync-accounts ]]; then
    note "Secrets are listed by name only (+ added, ~ value changed, - removed). Per-account files: $SECRETS_DIR (root-owned, 0640)."
    return
  fi
  if [[ "$ACTION" == tunnels ]]; then
    note "Public key and fingerprint only: the tunnel's private key is never printed. Units are listed by name."
    return
  fi
  if [[ "$ACTION" == swap ]]; then
    note "Swap: $SWAP_FILE, one '$SWAP_MARK' line in $SWAP_FSTAB, $SWAP_SYSCTL. Nothing else is touched."
    return
  fi
  if [[ "$ACTION" == backups ]]; then
    note "Secrets are listed by name only (+ added, ~ value changed, - removed). Backup secrets: $BACKUP_ENV_DST and the key file (root/backup user only)."
    return
  fi
  note "Variables are listed by name only. Secrets are in $ENV_FILE (root:root 600)."
  if [[ "$TIER" == t1 && "$PERIMETER" == public-https ]]; then
    note "Public host: the daemon sees the proxy's address for every client until WP-P4 (trusted-proxy) lands."
  fi
}

# ------------------------------------------------------------------ upgrade

semver_lt() {
  local v1="$1" v2="$2"
  if [[ "$v1" == "$v2" ]]; then return 1; fi
  local sorted
  sorted="$(printf '%s\n%s\n' "$v1" "$v2" | sort -V | head -1)"
  [[ "$sorted" == "$v1" ]]
}

count_open_terminals() {
  local svc="$1"
  if [[ -n "${IKENGA_TEST_OPEN_TERMINALS:-}" ]]; then
    echo "$IKENGA_TEST_OPEN_TERMINALS"
    return
  fi
  if ! command -v systemctl >/dev/null 2>&1; then
    echo "0"
    return
  fi
  if ! systemctl is-active --quiet "$svc" 2>/dev/null; then
    echo "0"
    return
  fi

  local pids=()
  local main_pid
  main_pid="$(systemctl show -p MainPID --value "$svc" 2>/dev/null || true)"
  if [[ -z "$main_pid" || "$main_pid" -le 0 ]]; then
    echo "0"
    return
  fi

  local cgroup
  cgroup="$(systemctl show -p ControlGroup --value "$svc" 2>/dev/null || true)"
  if [[ -n "$cgroup" && -f "/sys/fs/cgroup${cgroup}/cgroup.procs" ]]; then
    mapfile -t pids < "/sys/fs/cgroup${cgroup}/cgroup.procs" 2>/dev/null || true
  fi
  if [[ ${#pids[@]} -eq 0 ]]; then
    pids=("$main_pid")
    local children
    children="$(pgrep -P "$main_pid" 2>/dev/null || true)"
    for c in $children; do
      pids+=("$c")
      local gc
      gc="$(pgrep -P "$c" 2>/dev/null || true)"
      for g in $gc; do pids+=("$g"); done
    done
  fi

  local count=0
  local pts_list=()
  for p in "${pids[@]}"; do
    [[ -d "/proc/$p/fd" ]] || continue
    local pts
    pts="$(ls -l "/proc/$p/fd" 2>/dev/null | grep -oE '/dev/pts/[0-9]+' | sort -u || true)"
    if [[ -n "$pts" ]]; then
      while read -r line; do
        [[ -n "$line" ]] && pts_list+=("$line")
      done <<< "$pts"
    fi
  done
  if [[ ${#pts_list[@]} -gt 0 ]]; then
    count="$(printf '%s\n' "${pts_list[@]}" | sort -u | wc -l)"
  fi
  echo "$count"
}

get_latest_release_version() {
  local ver=""
  if command -v gh >/dev/null 2>&1; then
    ver="$(gh release view -R "$REPO" --json tagName -q .tagName 2>/dev/null | sed 's/^v//' || true)"
  fi
  if [[ ! "$ver" =~ $SEMVER_RE ]]; then
    ver="$(curl -fsSI "https://github.com/$REPO/releases/latest" 2>/dev/null | grep -i '^location:' | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
  fi
  if [[ ! "$ver" =~ $SEMVER_RE ]]; then
    ver="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null | grep -oE '"tag_name":\s*"v?[0-9]+\.[0-9]+\.[0-9]+"' | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
  fi
  [[ "$ver" =~ $SEMVER_RE ]] || ver=""
  echo "$ver"
}

# Top-level scalar fields of a JSON file, one per line, in the order asked
# ("" for a missing, null or non-scalar value; newlines inside a string
# become spaces so a value can never spill into the next field). Nothing here
# evaluates the file's text: every caller validates each line against a
# strict pattern before using it. Exits non-zero when the file isn't JSON.
json_fields() {
  local f="$1"; shift
  if command -v jq >/dev/null 2>&1; then
    jq -r 'if type == "object" then . else {} end | . as $d | $ARGS.positional[] | $d[.]
           | if type == "string" then gsub("[\r\n]"; " ")
             elif type == "number" or type == "boolean" then tostring
             else "" end' --args "$@" < "$f"
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c '
import json, sys
try:
    with open(sys.argv[1]) as fh:
        d = json.load(fh)
except Exception:
    sys.exit(3)
if not isinstance(d, dict):
    d = {}
for k in sys.argv[2:]:
    v = d.get(k)
    if isinstance(v, bool):
        print("true" if v else "false")
    elif isinstance(v, (int, float)):
        print(v)
    elif isinstance(v, str):
        print(v.replace("\r", " ").replace("\n", " "))
    else:
        print("")
' "$f" "$@"
  elif command -v node >/dev/null 2>&1; then
    node -e '
const fs = require("fs");
let d;
try { d = JSON.parse(fs.readFileSync(process.argv[1], "utf8")); } catch { process.exit(3); }
if (d === null || typeof d !== "object" || Array.isArray(d)) d = {};
for (const k of process.argv.slice(2)) {
  const v = d[k];
  if (typeof v === "string") console.log(v.replace(/[\r\n]/g, " "));
  else if (typeof v === "number" || typeof v === "boolean") console.log(String(v));
  else console.log("");
}
' "$f" "$@"
  else
    return 4
  fi
}

# The tarball artifact for one architecture: name, sha256, size (one per
# line, "" when absent). Same rules as json_fields.
manifest_artifact_fields() {
  local f="$1" arch="$2"
  if command -v jq >/dev/null 2>&1; then
    jq -r --arg a "$arch" '
      def s: if type == "string" then gsub("[\r\n]"; " ")
             elif type == "number" then tostring else "" end;
      ([(.artifacts // [])[]? | objects | select(.arch == $a and .kind == "tarball")] | .[0] // {}) as $t
      | ($t.name | s), ($t.sha256 | s), ($t.size | s)' < "$f"
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c '
import json, sys
with open(sys.argv[1]) as fh:
    d = json.load(fh)
arts = d.get("artifacts") if isinstance(d, dict) else None
if not isinstance(arts, list):
    arts = []
t = next((a for a in arts if isinstance(a, dict) and a.get("arch") == sys.argv[2] and a.get("kind") == "tarball"), {})
for k in ("name", "sha256", "size"):
    v = t.get(k)
    if isinstance(v, bool) or v is None or isinstance(v, (dict, list)):
        print("")
    else:
        print(str(v).replace("\r", " ").replace("\n", " "))
' "$f" "$arch"
  elif command -v node >/dev/null 2>&1; then
    node -e '
const d = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
const arts = d && Array.isArray(d.artifacts) ? d.artifacts : [];
const t = arts.find(a => a && typeof a === "object" && a.arch === process.argv[2] && a.kind === "tarball") || {};
for (const k of ["name", "sha256", "size"]) {
  const v = t[k];
  console.log(typeof v === "string" ? v.replace(/[\r\n]/g, " ") : typeof v === "number" ? String(v) : "");
}
' "$f" "$arch"
  else
    return 4
  fi
}

# fields_into ARRAY FILE KEY... : json_fields into ARRAY, one element per
# key. `$(...)` strips trailing newlines, which would drop trailing empty
# fields, hence the sentinel.
fields_into() {
  local -n _dst="$1"; shift
  local _out
  _out="$(json_fields "$@" && printf '.')" || return 1
  _out="${_out%.}"
  mapfile -t _dst < <(printf '%s' "$_out")
}

# Parse and validate a release manifest into SCHEMA, MANIFEST_VERSION,
# MANIFEST_CHANNEL, MIN_UPGRADE_FROM, PUBLISHED_AT, ARTIFACT_NAME,
# ARTIFACT_SHA and ARTIFACT_SIZE. Every value must match a strict pattern, or
# this returns 1 with a fixed phrase in MANIFEST_ERROR. The manifest's own
# text is never echoed and never run: an earlier version `eval`ed it, which
# was root code execution for whoever could serve the manifest.
parse_manifest() {
  local manifest_file="$1" arch="$2" out
  local -a top art
  MANIFEST_ERROR=""
  if ! fields_into top "$manifest_file" schema version channel min_upgrade_from published_at 2>/dev/null; then
    MANIFEST_ERROR="the release manifest is not valid JSON (or no jq, python3 or node to read it)"; return 1
  fi
  if ! out="$(manifest_artifact_fields "$manifest_file" "$arch" 2>/dev/null && printf '.')"; then
    MANIFEST_ERROR="the release manifest is not valid JSON"; return 1
  fi
  out="${out%.}"
  mapfile -t art < <(printf '%s' "$out")
  if [[ ${#top[@]} -ne 5 || ${#art[@]} -ne 3 ]]; then MANIFEST_ERROR="the release manifest is malformed"; return 1; fi

  SCHEMA="${top[0]}" MANIFEST_VERSION="${top[1]}" MANIFEST_CHANNEL="${top[2]}"
  MIN_UPGRADE_FROM="${top[3]}" PUBLISHED_AT="${top[4]}"
  ARTIFACT_NAME="${art[0]}" ARTIFACT_SHA="${art[1]}" ARTIFACT_SIZE="${art[2]}"

  if [[ "$SCHEMA" != "ikenga-server-release/1" ]]; then
    MANIFEST_ERROR="the release manifest has an unsupported schema (expected ikenga-server-release/1)"; return 1
  fi
  if [[ ! "$MANIFEST_VERSION" =~ $SEMVER_RE || ${#MANIFEST_VERSION} -gt 32 ]]; then
    MANIFEST_ERROR="the release manifest has an invalid version"; return 1
  fi
  if [[ ! "$MANIFEST_CHANNEL" =~ ^(stable|next)$ ]]; then
    MANIFEST_ERROR="the release manifest has an invalid channel"; return 1
  fi
  if [[ -n "$MIN_UPGRADE_FROM" && ( ! "$MIN_UPGRADE_FROM" =~ $SEMVER_RE || ${#MIN_UPGRADE_FROM} -gt 32 ) ]]; then
    MANIFEST_ERROR="the release manifest has an invalid min_upgrade_from"; return 1
  fi
  if [[ -n "$PUBLISHED_AT" && ! "$PUBLISHED_AT" =~ ^[0-9TZ:.+-]{1,40}$ ]]; then
    MANIFEST_ERROR="the release manifest has an invalid published_at"; return 1
  fi
  if [[ -n "$ARTIFACT_NAME" && "$ARTIFACT_NAME" != "ikenga-server_${MANIFEST_VERSION}_linux_${arch}.tar.gz" ]]; then
    MANIFEST_ERROR="the release manifest names an unexpected tarball for $arch"; return 1
  fi
  if [[ -n "$ARTIFACT_SHA" && ! "$ARTIFACT_SHA" =~ ^[0-9a-f]{64}$ ]]; then
    MANIFEST_ERROR="the release manifest has an invalid sha256 for $arch"; return 1
  fi
  if [[ -n "$ARTIFACT_SIZE" && ! "$ARTIFACT_SIZE" =~ ^[0-9]{1,12}$ ]]; then
    MANIFEST_ERROR="the release manifest has an invalid size for $arch"; return 1
  fi
  return 0
}

installed_version() {
  local v=""
  if [[ -x "$INSTALL_DIR/bin/ikenga-server" ]]; then
    v="$("$INSTALL_DIR/bin/ikenga-server" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
  fi
  printf '%s' "$v"
}

# The address the daemon answers /api/health on. IKENGA_HOST is the bind
# address provision wrote to the env file: a tailnet box binds only its
# tailnet IP, so 127.0.0.1 never answers there and every upgrade would roll
# back. Only that one line is read; the file is never sourced.
health_base_url() {
  local h=""
  if [[ -n "${IKENGA_HOST_VALUE:-}" ]]; then
    h="$IKENGA_HOST_VALUE"
  elif [[ -r "$ENV_FILE" ]]; then
    h="$(grep -E '^IKENGA_HOST=' "$ENV_FILE" 2>/dev/null | tail -1 | cut -d= -f2- || true)"
    h="${h%\"}"; h="${h#\"}"
  fi
  if [[ "$h" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ || "$h" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]{0,252})$ ]] \
     && [[ "$h" != "0.0.0.0" ]]; then
    printf 'http://%s:4000' "$h"
  else
    printf 'http://127.0.0.1:4000'
  fi
}

# /api/health answers ok:true AND reports version $2.
health_reports() {
  local url="$1" want="${2//./\\.}"
  local body
  body="$(curl -fsS -m 2 "$url/api/health" 2>/dev/null)" || return 1
  grep -q '"ok":[[:space:]]*true' <<<"$body" && grep -qE "\"version\":[[:space:]]*\"$want\"" <<<"$body"
}

# One upgrade at a time, shared by `upgrade` and `apply-request` (fd 9);
# check-update has its own lock (fd 8).
ensure_state_dir() {
  [[ -d "$STATE_DIR" ]] || install -d -m 0755 -o root -g root "$STATE_DIR"
}
take_upgrade_lock() {
  ensure_state_dir
  exec 9>"$STATE_DIR/upgrade.lock"
  flock -n 9
}

do_upgrade() {
  log "Upgrade ikenga-server"
  [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo); dry runs may be unprivileged"

  detect_arch

  local have=""
  have="$(installed_version)"
  [[ -n "$have" ]] || die "no installed ikenga-server found at $INSTALL_DIR/bin/ikenga-server; cannot upgrade"
  note "installed version: $have ($ARCH)"

  local unit="ikenga-server.service"
  [[ "${TIER:-t1}" == t1 ]] && unit="ikenga-server-t1.service"
  local svc="${unit%.service}"

  local open_terms
  open_terms="$(count_open_terminals "$svc")"
  if (( open_terms > 0 )) && [[ $FORCE -ne 1 ]]; then
    die "$open_terms open terminal(s) detected. Upgrading will terminate open terminals. Use --force to proceed anyway."
  fi
  if (( open_terms > 0 )) && [[ $FORCE -eq 1 ]]; then
    note "Warning: $open_terms open terminal(s) will be terminated (--force specified)"
  fi

  local channel="${CHANNEL:-stable}"
  local base_url="${RELEASE_BASE_URL:-https://github.com/$REPO/releases/download}"
  local target="$TARGET_VERSION"

  if [[ -z "$target" || $UPGRADE_LATEST -eq 1 ]]; then
    log "Checking latest release on channel '$channel'..."
    if [[ -z "${RELEASE_MANIFEST_URL:-}" ]]; then
      local latest_ver
      latest_ver="$(get_latest_release_version)"
      [[ -n "$latest_ver" ]] || die "could not determine latest release version from $REPO; specify --to X.Y.Z"
      target="$latest_ver"
    fi
  fi

  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  local manifest_file="$tmp/manifest.json"
  if [[ -n "${RELEASE_MANIFEST_URL:-}" ]]; then
    note "Fetching manifest from $RELEASE_MANIFEST_URL"
    if [[ "$RELEASE_MANIFEST_URL" =~ ^https?:// ]]; then
      curl -fsSL -o "$manifest_file" "$RELEASE_MANIFEST_URL"
    else
      cp -a "$RELEASE_MANIFEST_URL" "$manifest_file"
    fi
  elif [[ -n "$target" ]]; then
    local murl="$base_url/v$target/ikenga-server_${target}_manifest.json"
    note "Fetching manifest for v$target ($channel) from $murl"
    curl -fsSL -o "$manifest_file" "$murl"
  else
    die "target version is required"
  fi

  local SCHEMA="" MANIFEST_VERSION="" MANIFEST_CHANNEL="" MIN_UPGRADE_FROM="" PUBLISHED_AT=""
  local ARTIFACT_NAME="" ARTIFACT_SHA="" ARTIFACT_SIZE="" MANIFEST_ERROR=""
  parse_manifest "$manifest_file" "$ARCH" || die "$MANIFEST_ERROR"

  # The version asked for is the version installed. The manifest only ever
  # confirms it: it never picks a different one (it used to, silently).
  if [[ -n "$TARGET_VERSION" && $UPGRADE_LATEST -ne 1 && "$MANIFEST_VERSION" != "$TARGET_VERSION" ]]; then
    die "the manifest is for $MANIFEST_VERSION, not the requested $TARGET_VERSION; nothing installed"
  fi
  target="$MANIFEST_VERSION"

  if [[ "$MANIFEST_CHANNEL" != "$channel" ]]; then
    die "manifest channel '$MANIFEST_CHANNEL' does not match profile channel '$channel'"
  fi

  if [[ "$have" == "$target" ]]; then
    note "already on version $target ($channel channel); nothing to upgrade"
    printf 'noop: already on %s\n' "$target"
    return 0
  fi

  if semver_lt "$target" "$have"; then
    die "$target is older than the installed $have; downgrades are not supported here"
  fi

  if [[ -n "$MIN_UPGRADE_FROM" ]] && semver_lt "$have" "$MIN_UPGRADE_FROM"; then
    die "installed version $have is below min-upgrade-from $MIN_UPGRADE_FROM for $target; upgrade to an intermediate version first"
  fi

  [[ -n "$ARTIFACT_NAME" && -n "$ARTIFACT_SHA" ]] || die "no tarball artifact for $ARCH found in manifest for $target"

  note "target version: $target (channel: $channel, min-upgrade-from: ${MIN_UPGRADE_FROM:-none})"
  note "artifact: $ARTIFACT_NAME (sha256: $ARTIFACT_SHA)"

  if [[ $DRY_RUN -eq 1 ]]; then
    note "[dry-run] download $ARTIFACT_NAME"
    note "[dry-run] verify sha256 checksum ($ARTIFACT_SHA)"
    note "[dry-run] keep previous binary as bin/ikenga-server.prev-$have"
    note "[dry-run] swap binary to $target in $INSTALL_DIR/bin/"
    note "[dry-run] restart service $svc"
    note "[dry-run] health check /api/health with automatic rollback to $have on failure"
    changed "ikenga-server upgraded $have -> $target"
    summary
    return 0
  fi

  local tarball_path="$tmp/$ARTIFACT_NAME"
  if [[ -n "${RELEASE_TARBALL_PATH:-}" && -f "$RELEASE_TARBALL_PATH" ]]; then
    cp -a "$RELEASE_TARBALL_PATH" "$tarball_path"
  else
    local tarball_url="$base_url/v$target/$ARTIFACT_NAME"
    note "Downloading $tarball_url"
    curl -fsSL -o "$tarball_path" "$tarball_url"
  fi

  note "Verifying checksum..."
  local actual_sha
  actual_sha="$(sha256sum "$tarball_path" | awk '{print $1}')"
  if [[ "$actual_sha" != "$ARTIFACT_SHA" ]]; then
    die "checksum mismatch for $ARTIFACT_NAME: expected $ARTIFACT_SHA, got $actual_sha; nothing installed"
  fi
  note "checksum OK ($actual_sha)"

  if command -v gh >/dev/null 2>&1; then
    gh attestation verify "$tarball_path" -R "$REPO" >/dev/null 2>&1 || note "WARNING: attestation not verified (gh not authenticated or failed); checksum matched"
  fi

  cp -a "$INSTALL_DIR/bin/ikenga-server" "$INSTALL_DIR/bin/ikenga-server.prev-$have"
  note "previous binary saved as bin/ikenga-server.prev-$have (rollback guard)"

  mkdir -p "$tmp/stage"
  tar -xzf "$tarball_path" -C "$tmp/stage"
  [[ -x "$tmp/stage/bin/ikenga-server" ]] || die "tarball $ARTIFACT_NAME missing bin/ikenga-server"

  install -m 0755 "$tmp/stage/bin/ikenga-server" "$INSTALL_DIR/bin/ikenga-server"

  if [[ -d "$tmp/stage/dist" ]]; then
    rm -rf "$INSTALL_DIR/dist.prev-$have"
    [[ -d "$INSTALL_DIR/dist" ]] && mv "$INSTALL_DIR/dist" "$INSTALL_DIR/dist.prev-$have"
    cp -r "$tmp/stage/dist" "$INSTALL_DIR/dist"
  fi

  # Restore only a unit THIS run saved: a leftover .prev from an earlier
  # attempt must never be put back over the current unit.
  local unit_saved=0
  rm -f "$SYSTEMD_DIR/$unit.prev-$have"
  if [[ -f "$tmp/stage/$unit" ]]; then
    if [[ -f "$SYSTEMD_DIR/$unit" ]]; then
      cp -a "$SYSTEMD_DIR/$unit" "$SYSTEMD_DIR/$unit.prev-$have"
      unit_saved=1
      note "previous unit saved as $SYSTEMD_DIR/$unit.prev-$have (rollback guard)"
    fi
    install -m 0644 "$tmp/stage/$unit" "$SYSTEMD_DIR/$unit"
    command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload 2>/dev/null || true
  fi

  log "Restarting $svc..."
  if command -v systemctl >/dev/null 2>&1; then
    systemctl restart "$svc" 2>/dev/null || true
  fi

  local url
  url="$(health_base_url)"
  log "Checking health at $url/api/health (expecting version $target)..."

  # Healthy means the NEW binary answers: a stale process still serving the
  # old version must not count as a successful upgrade.
  local healthy=0
  for _ in $(seq 1 "${HEALTH_TIMEOUT:-20}"); do
    if health_reports "$url" "$target"; then
      healthy=1
      break
    fi
    sleep 1
  done

  if [[ $healthy -eq 1 ]]; then
    log "Health check PASSED: ikenga-server $target is healthy at $url"
    changed "ikenga-server upgraded $have -> $target"
    summary
    return 0
  fi

  log "WARNING: /api/health failed after upgrade to $target! Initiating automatic rollback to $have..."
  cp -a "$INSTALL_DIR/bin/ikenga-server.prev-$have" "$INSTALL_DIR/bin/ikenga-server"
  if [[ -d "$INSTALL_DIR/dist.prev-$have" ]]; then
    rm -rf "$INSTALL_DIR/dist"
    mv "$INSTALL_DIR/dist.prev-$have" "$INSTALL_DIR/dist"
  fi
  if [[ $unit_saved -eq 1 && -f "$SYSTEMD_DIR/$unit.prev-$have" ]]; then
    cp -a "$SYSTEMD_DIR/$unit.prev-$have" "$SYSTEMD_DIR/$unit"
    command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload 2>/dev/null || true
  fi
  if command -v systemctl >/dev/null 2>&1; then
    systemctl restart "$svc" 2>/dev/null || true
  fi

  local rolled_back=0
  for _ in $(seq 1 "${HEALTH_TIMEOUT:-20}"); do
    if health_reports "$url" "$have"; then
      rolled_back=1
      break
    fi
    sleep 1
  done

  # Distinct exit codes, so a caller (apply-request) can tell a clean
  # rollback (3) from a box that needs a human (4).
  if [[ $rolled_back -eq 1 ]]; then
    printf 'error: upgrade to %s failed health check; automatically rolled back to %s successfully\n' "$target" "$have" >&2
    exit 3
  else
    printf 'error: upgrade to %s failed health check AND rollback to %s also failed; see journalctl -u %s\n' "$target" "$have" "$svc" >&2
    exit 4
  fi
}

# ---------------------------------------------------------- in-app updates
#
# Files (root-owned STATE_DIR, 0755; the server reads, never writes):
#   available.json  ikenga-update-available/1, written by check-update
#   status.json     ikenga-update-status/1, written by apply-request
#   last-run.log    0600, the last apply's full output
# The request (ikenga-update-request/1) is written by the server:
#   t0: $INSTALL_DIR/data/update-request.json          (owner: ikenga)
#   t1: $INSTALL_DIR/data/operator/update-request.json (owner: root, the broker)

update_request_path() {
  if [[ "$TIER" == t1 ]]; then
    printf '%s/data/operator/update-request.json' "$INSTALL_DIR"
  else
    printf '%s/data/update-request.json' "$INSTALL_DIR"
  fi
}

now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# A JSON string literal for a value that already passed a strict pattern, or
# null when empty. Escapes anyway, and drops control characters, so a value
# can never break the document.
jstr() {
  local v="$1"
  if [[ -z "$v" ]]; then printf 'null'; return; fi
  v="${v//\\/\\\\}"; v="${v//\"/\\\"}"
  v="$(printf '%s' "$v" | tr -d '\000-\037')"
  printf '"%s"' "$v"
}

# Write $2 to $STATE_DIR/$1 atomically (0644).
write_state_file() {
  local name="$1" content="$2" tmp
  ensure_state_dir
  tmp="$(mktemp "$STATE_DIR/.$name.XXXXXX")"
  printf '%s\n' "$content" > "$tmp"
  chmod 0644 "$tmp"
  mv -fT -- "$tmp" "$STATE_DIR/$name"
}

# Seconds since an ISO-8601 time we wrote ourselves; empty on failure.
age_of() {
  local t="$1" epoch
  [[ "$t" =~ ^[0-9TZ:.+-]{1,40}$ ]] || return 0
  epoch="$(date -u -d "$t" +%s 2>/dev/null)" || return 0
  printf '%s' "$(( $(date -u +%s) - epoch ))"
}

# Read-only: never installs anything. Exit 0 even on a handled failure, so a
# flaky network does not mark the timer's service failed.
do_check_update() {
  [[ $EUID -eq 0 ]] || die "check-update must run as root"
  ensure_state_dir
  exec 8>"$STATE_DIR/check.lock"
  flock -n 8 || { note "another update check is running"; return 0; }
  detect_arch

  local installed; installed="$(installed_version)"
  local channel="${CHANNEL:-stable}"
  local latest="" min="" published="" blocked=false blocked_reason="" err=""
  local tmp; tmp="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" RETURN
  local manifest_file="$tmp/manifest.json"

  if [[ -n "${RELEASE_MANIFEST_URL:-}" ]]; then
    if [[ "$RELEASE_MANIFEST_URL" =~ ^https?:// ]]; then
      curl -fsSL -m 30 -o "$manifest_file" "$RELEASE_MANIFEST_URL" 2>/dev/null || err="could not reach the release server"
    else
      cp -- "$RELEASE_MANIFEST_URL" "$manifest_file" 2>/dev/null || err="could not reach the release server"
    fi
  else
    latest="$(get_latest_release_version)"
    if [[ -z "$latest" ]]; then
      err="could not reach the release server"
    else
      curl -fsSL -m 30 -o "$manifest_file" \
        "$RELEASE_BASE_URL/v$latest/ikenga-server_${latest}_manifest.json" 2>/dev/null \
        || err="could not reach the release server"
    fi
  fi

  local SCHEMA="" MANIFEST_VERSION="" MANIFEST_CHANNEL="" MIN_UPGRADE_FROM="" PUBLISHED_AT=""
  local ARTIFACT_NAME="" ARTIFACT_SHA="" ARTIFACT_SIZE="" MANIFEST_ERROR=""
  if [[ -z "$err" ]]; then
    if ! parse_manifest "$manifest_file" "$ARCH"; then
      err="the release manifest could not be read"
    elif [[ -n "$latest" && "$MANIFEST_VERSION" != "$latest" ]]; then
      err="the release manifest does not match the latest release"
    elif [[ "$MANIFEST_CHANNEL" != "$channel" ]]; then
      err="the latest release is not on this server's channel"
    elif [[ -z "$ARTIFACT_NAME" || -z "$ARTIFACT_SHA" ]]; then
      err="the latest release has no build for this architecture"
    fi
  fi

  if [[ -z "$err" ]]; then
    latest="$MANIFEST_VERSION"; min="$MIN_UPGRADE_FROM"; published="$PUBLISHED_AT"
  else
    # Keep what the last good check found; only last_error changes. Every
    # field read back is re-validated: the file is ours, but trust nothing.
    latest="" min="" published=""
    if [[ -f "$STATE_DIR/available.json" ]]; then
      local -a prev
      if fields_into prev "$STATE_DIR/available.json" latest min_upgrade_from published_at 2>/dev/null; then
        [[ "${prev[0]:-}" =~ $SEMVER_RE ]] && latest="${prev[0]}"
        [[ "${prev[1]:-}" =~ $SEMVER_RE ]] && min="${prev[1]}"
        [[ "${prev[2]:-}" =~ ^[0-9TZ:.+-]{1,40}$ ]] && published="${prev[2]}"
      fi
    fi
    note "update check: $err"
  fi

  if [[ -n "$latest" && -n "$min" && -n "$installed" ]] && semver_lt "$installed" "$min"; then
    blocked=true
    blocked_reason="requires $min first"
  fi
  local notes_url=""
  # Built here from the validated version, never copied from the manifest.
  [[ -n "$latest" ]] && notes_url="https://github.com/$REPO/releases/tag/v$latest"

  write_state_file available.json "{\"schema\":\"ikenga-update-available/1\",\"checked_at\":$(jstr "$(now_iso)"),\"channel\":$(jstr "$channel"),\"installed\":$(jstr "$installed"),\"latest\":$(jstr "$latest"),\"min_upgrade_from\":$(jstr "$min"),\"blocked\":$blocked,\"blocked_reason\":$(jstr "$blocked_reason"),\"notes_url\":$(jstr "$notes_url"),\"published_at\":$(jstr "$published"),\"last_error\":$(jstr "$err")}"
  note "update check: installed ${installed:-unknown}, latest ${latest:-unknown}${err:+ (error: $err)}"
  return 0
}

# status.json. Arguments are name=value pairs; values must already be safe.
write_status() {
  local state="" from="" to="" request_id="" requested_by="" started_at="" finished_at=""
  local rolled_back=false exit_code="" message="" log_tail="[]" kv
  for kv in "$@"; do
    case "$kv" in
      state=*) state="${kv#*=}" ;;
      from=*) from="${kv#*=}" ;;
      to=*) to="${kv#*=}" ;;
      request_id=*) request_id="${kv#*=}" ;;
      requested_by=*) requested_by="${kv#*=}" ;;
      started_at=*) started_at="${kv#*=}" ;;
      finished_at=*) finished_at="${kv#*=}" ;;
      rolled_back=*) rolled_back="${kv#*=}" ;;
      exit_code=*) exit_code="${kv#*=}" ;;
      message=*) message="${kv#*=}" ;;
      log_tail=*) log_tail="${kv#*=}" ;;
    esac
  done
  [[ "$exit_code" =~ ^[0-9]{1,3}$ ]] || exit_code=null
  write_state_file status.json "{\"schema\":\"ikenga-update-status/1\",\"state\":$(jstr "$state"),\"from\":$(jstr "$from"),\"to\":$(jstr "$to"),\"request_id\":$(jstr "$request_id"),\"requested_by\":$(jstr "$requested_by"),\"started_at\":$(jstr "$started_at"),\"finished_at\":$(jstr "$finished_at"),\"rolled_back\":$rolled_back,\"exit_code\":$exit_code,\"message\":$(jstr "$message"),\"log_tail\":$log_tail}"
}

# The last 40 progress lines of the run as a JSON array. Only provision.sh's
# own progress shapes pass, each cut to 300 characters; do_upgrade prints no
# environment values and the .env is never read on this path.
log_tail_json() {
  local f="$1" line out="" first=1
  [[ -f "$f" ]] || { printf '[]'; return; }
  while IFS= read -r line; do
    line="$(printf '%s' "$line" | tr -d '\000-\037' | cut -c1-300)"
    line="${line//\\/\\\\}"; line="${line//\"/\\\"}"
    if [[ $first -eq 1 ]]; then first=0; else out+=","; fi
    out+="\"$line\""
  done < <(grep -E '^(==>|    |error:|WARNING|noop:)' "$f" | tail -n 40 || true)
  printf '[%s]' "$out"
}

# Run by ikenga-update.service when the server drops a request. The request
# is untrusted input: it may select nothing except "apply the version root
# itself advertised, now". Root decides everything else from its own state.
do_apply_request() {
  [[ $EUID -eq 0 ]] || die "apply-request must run as root"
  ensure_state_dir
  local req claim cf
  req="$(update_request_path)"
  claim="$INSTALL_DIR/.update-claim"
  install -d -m 0700 -o root -g root "$claim"
  cf="$claim/request.json"
  rm -rf -- "$cf"

  # Claim by rename into a root-only directory: from here on nobody else can
  # swap, relink or rewrite the name we check. rename(2) moves a symlink
  # without following it.
  if ! mv -fT -- "$req" "$cf" 2>/dev/null; then
    note "no update request to apply"
    return 0
  fi

  local started; started="$(now_iso)"
  local installed; installed="$(installed_version)"

  refuse() {
    # $1 message, $2 request_id (validated or empty), $3 requested_by, $4 to
    rm -rf -- "$cf"
    write_status state=refused from="$installed" to="${4:-}" request_id="${2:-}" requested_by="${3:-}" \
      started_at="$started" finished_at="$(now_iso)" message="$1"
    printf 'ikenga-update: request %s by %s refused: %s\n' "${2:-?}" "${3:-?}" "$1"
  }

  # Check the claimed file BEFORE reading it.
  if [[ -L "$cf" || ! -f "$cf" ]]; then refuse "invalid request"; return 0; fi
  local owner nlink size mtime expected_owner
  read -r owner nlink size mtime < <(stat -c '%u %h %s %Y' -- "$cf")
  if [[ "$TIER" == t1 ]]; then
    expected_owner=0
  else
    expected_owner="$(id -u ikenga 2>/dev/null || true)"
  fi
  if [[ -z "$expected_owner" || "$owner" != "$expected_owner" || "$nlink" != 1 ]] \
     || (( size <= 0 || size > 4096 )); then
    refuse "invalid request"; return 0
  fi
  local now; now="$(date -u +%s)"
  if (( now - mtime > UPDATE_REQUEST_MAX_AGE || mtime - now > 60 )); then
    refuse "the request expired"; return 0
  fi

  # Read ONCE, then validate only the copy: whatever happens to the file
  # (or an fd someone kept open on it) after this no longer matters.
  local content parsed
  content="$(head -c 4096 -- "$cf")"
  rm -rf -- "$cf"
  parsed="$(mktemp "$claim/.parse.XXXXXX")"
  printf '%s' "$content" > "$parsed"
  local -a f
  if ! fields_into f "$parsed" schema version request_id requested_by requested_at acknowledged_open_terminals 2>/dev/null; then
    rm -f -- "$parsed"; refuse "invalid request"; return 0
  fi
  rm -f -- "$parsed"
  local schema="${f[0]:-}" version="${f[1]:-}" request_id="${f[2]:-}" requested_by="${f[3]:-}"
  local requested_at="${f[4]:-}" ack="${f[5]:-}"
  [[ "$requested_by" =~ ^[A-Za-z0-9._-]{1,64}$ ]] || requested_by="unknown"
  if [[ ${#f[@]} -ne 6 || "$schema" != "ikenga-update-request/1" \
        || ! "$version" =~ $SEMVER_RE || ${#version} -gt 32 \
        || ! "$request_id" =~ ^[0-9a-f-]{36}$ \
        || ! "$ack" =~ ^[0-9]{1,4}$ ]]; then
    refuse "invalid request" "" "$requested_by"; return 0
  fi
  local req_age; req_age="$(age_of "$requested_at")"
  if [[ -z "$req_age" ]] || (( req_age > UPDATE_REQUEST_MAX_AGE || req_age < -60 )); then
    refuse "the request expired" "$request_id" "$requested_by" "$version"; return 0
  fi

  # One upgrade at a time (shared with a manual `upgrade` over SSH).
  if ! take_upgrade_lock; then
    refuse "another upgrade is running" "$request_id" "$requested_by" "$version"; return 0
  fi

  # Cross-check against root's own state only.
  local -a av
  if [[ ! -f "$STATE_DIR/available.json" ]] || ! fields_into av "$STATE_DIR/available.json" latest blocked 2>/dev/null; then
    refuse "no update has been advertised" "$request_id" "$requested_by" "$version"; return 0
  fi
  if [[ "${av[0]:-}" != "$version" ]]; then
    refuse "that version is not the advertised update" "$request_id" "$requested_by" "$version"; return 0
  fi
  if [[ "${av[1]:-}" == true ]]; then
    refuse "the advertised update is blocked; update over SSH" "$request_id" "$requested_by" "$version"; return 0
  fi
  if [[ -z "$installed" ]]; then
    refuse "no installed ikenga-server found" "$request_id" "$requested_by" "$version"; return 0
  fi
  if [[ "$installed" == "$version" ]]; then
    write_status state=noop from="$installed" to="$version" request_id="$request_id" requested_by="$requested_by" \
      started_at="$started" finished_at="$(now_iso)" message="already on $version"
    printf 'ikenga-update: request %s by %s: already on %s (noop)\n' "$request_id" "$requested_by" "$version"
    return 0
  fi
  if semver_lt "$version" "$installed"; then
    refuse "that version is older than the installed one" "$request_id" "$requested_by" "$version"; return 0
  fi
  local -a st=()
  if [[ -f "$STATE_DIR/status.json" ]] && fields_into st "$STATE_DIR/status.json" state to finished_at 2>/dev/null; then
    if [[ ( "${st[0]:-}" == rolled_back || "${st[0]:-}" == failed ) && "${st[1]:-}" == "$version" ]]; then
      local fin_age; fin_age="$(age_of "${st[2]:-}")"
      if [[ -n "$fin_age" ]] && (( fin_age < UPDATE_RETRY_COOLDOWN )); then
        refuse "cooldown: the last attempt at $version failed less than an hour ago" "$request_id" "$requested_by" "$version"
        return 0
      fi
    fi
  fi

  write_status state=running from="$installed" to="$version" request_id="$request_id" \
    requested_by="$requested_by" started_at="$started"
  printf 'ikenga-update: request %s by %s: upgrading %s -> %s\n' "$request_id" "$requested_by" "$installed" "$version"

  # FORCE: the server already made the admin acknowledge the open terminals
  # it counted; root's /dev/pts heuristic over-counts (detached runners).
  local rc log="$STATE_DIR/last-run.log"
  ( umask 077; : > "$log" )
  chmod 0600 "$log"
  set +e
  # shellcheck disable=SC2030  # the overrides are meant for this subshell only
  ( set -e; TARGET_VERSION="$version"; UPGRADE_LATEST=0; FORCE=1; DRY_RUN=0; do_upgrade ) >>"$log" 2>&1
  rc=$?
  set -e

  local state message rolled_back=false
  case "$rc" in
    0)
      if grep -q '^noop:' "$log"; then state=noop; message="already on $version"
      else state=succeeded; message="updated to $version"; fi ;;
    3) state=rolled_back; rolled_back=true; message="the update failed its health check and was rolled back to $installed" ;;
    4) state=failed; message="rollback also failed; see journalctl" ;;
    *) state=failed; message="the update did not start; nothing was changed" ;;
  esac
  write_status state="$state" from="$installed" to="$version" request_id="$request_id" \
    requested_by="$requested_by" started_at="$started" finished_at="$(now_iso)" \
    rolled_back="$rolled_back" exit_code="$rc" message="$message" log_tail="$(log_tail_json "$log")"
  printf 'ikenga-update: request %s by %s: %s -> %s: %s\n' "$request_id" "$requested_by" "$installed" "$version" "$state"

  # Re-check, so available.json reflects what is installed now.
  ( do_check_update ) >/dev/null 2>&1 || true
  return 0
}

# Write $SYSTEMD_DIR/$1 only when its content differs.
write_unit() {
  local name="$1" content="$2"
  if [[ "$(cat "$SYSTEMD_DIR/$name" 2>/dev/null || true)" == "${content%$'\n'}" ]]; then return 1; fi
  if [[ $DRY_RUN -eq 1 ]]; then
    printf '    [dry-run] write %s/%s:\n' "$SYSTEMD_DIR" "$name"
    printf '%s\n' "$content" | sed 's/^/      | /'
  else
    printf '%s' "$content" > "$SYSTEMD_DIR/$name"
    chmod 0644 "$SYSTEMD_DIR/$name"
  fi
  changed "unit $name installed"
  return 0
}

# The stable copy of this script and the four update units. Idempotent.
install_update_units() {
  log "Update units (check timer, request path, stable provisioner copy)"
  [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo); dry runs may be unprivileged"
  local self req
  self="$(readlink -f "${BASH_SOURCE[0]}")"
  req="$(update_request_path)"

  if [[ "$self" != "$(readlink -f "$STABLE_COPY" 2>/dev/null || true)" ]] && ! cmp -s "$self" "$STABLE_COPY"; then
    run install -D -m 0755 -o root -g root "$self" "$STABLE_COPY"
    changed "stable provisioner copy at $STABLE_COPY"
  fi

  local any=0
  write_unit ikenga-update-check.service "[Unit]
Description=Ikenga: check for a server update (notify only; installs nothing)
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=$STABLE_COPY check-update
Nice=10
StateDirectory=ikenga-update
StateDirectoryMode=0755
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
" && any=1
  write_unit ikenga-update-check.timer "[Unit]
Description=Ikenga: daily server update check

[Timer]
OnBootSec=15min
OnCalendar=daily
RandomizedDelaySec=6h
Persistent=true

[Install]
WantedBy=timers.target
" && any=1
  write_unit ikenga-update.path "[Unit]
Description=Ikenga: apply an admin's server update request

[Path]
PathExists=$req
Unit=ikenga-update.service
TriggerLimitIntervalSec=60
TriggerLimitBurst=5

[Install]
WantedBy=multi-user.target
" && any=1
  write_unit ikenga-update.service "[Unit]
Description=Ikenga: apply a requested server update (upgrade, health check, automatic rollback)

[Service]
Type=oneshot
ExecStart=$STABLE_COPY apply-request
TimeoutStartSec=15min
StateDirectory=ikenga-update
StateDirectoryMode=0755
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=$INSTALL_DIR $SYSTEMD_DIR
ProtectHome=read-only
" && any=1

  if [[ $DRY_RUN -eq 1 ]]; then
    note "[dry-run] systemctl daemon-reload; enable --now ikenga-update-check.timer ikenga-update.path"
    return 0
  fi
  install -d -m 0755 -o root -g root "$STATE_DIR"
  if command -v systemctl >/dev/null 2>&1; then
    [[ $any -eq 1 ]] && systemctl daemon-reload
    systemctl enable --now ikenga-update-check.timer ikenga-update.path >/dev/null 2>&1 \
      || note "WARNING: could not enable the update timer/path units (see systemctl status ikenga-update.path)"
    # First check now, not 15 minutes after the next boot.
    [[ ! -f "$STATE_DIR/available.json" ]] && systemctl start --no-block ikenga-update-check.service >/dev/null 2>&1 || true
  fi
}

# The npm package behind each npm-installed AGENT_CLI (agy uses its own
# installer and is not updated here).
agent_cli_npm_pkg() {
  case "$1" in
    claude) echo @anthropic-ai/claude-code ;;
    codex) echo @openai/codex ;;
    opencode) echo opencode-ai ;;
    pi) echo @earendil-works/pi-coding-agent ;;
  esac
}

# A daily timer that keeps the npm-installed agent CLIs current. They are
# installed system-wide as root, so a principal's own CLI cannot self-update
# (claude shows "Auto-update failed" in every terminal). Running sessions keep
# their binary; new ones get the update. Idempotent.
install_agent_cli_updates() {
  local cli pkg pkgs=()
  for cli in "${AGENT_CLIS[@]}"; do
    pkg="$(agent_cli_npm_pkg "$cli")"
    [[ -n "$pkg" ]] && pkgs+=("$pkg@latest")
  done
  [[ ${#pkgs[@]} -gt 0 ]] || return 0
  log "Agent CLI updates (daily npm update: ${pkgs[*]})"
  [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo); dry runs may be unprivileged"
  local npm any=0
  npm="$(command -v npm || echo /usr/bin/npm)"

  write_unit ikenga-agent-cli-update.service "[Unit]
Description=Ikenga: update the system-wide agent CLIs (${AGENT_CLIS[*]})
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=$npm install -g --no-fund --no-audit ${pkgs[*]}
TimeoutStartSec=15min
Nice=10
CacheDirectory=ikenga-npm
Environment=npm_config_cache=/var/cache/ikenga-npm
NoNewPrivileges=true
PrivateTmp=true
" && any=1
  write_unit ikenga-agent-cli-update.timer "[Unit]
Description=Ikenga: daily agent CLI update

[Timer]
OnBootSec=20min
OnCalendar=daily
RandomizedDelaySec=6h
Persistent=true

[Install]
WantedBy=timers.target
" && any=1

  if [[ $DRY_RUN -eq 1 ]]; then
    note "[dry-run] systemctl daemon-reload; enable --now ikenga-agent-cli-update.timer"
    return 0
  fi
  if command -v systemctl >/dev/null 2>&1; then
    [[ $any -eq 1 ]] && systemctl daemon-reload
    systemctl enable --now ikenga-agent-cli-update.timer >/dev/null 2>&1 \
      || note "WARNING: could not enable ikenga-agent-cli-update.timer (see systemctl status ikenga-agent-cli-update.timer)"
  fi
}

# ------------------------------------------- accounts: projects and secrets
#
# Under T1 every person and every agent is a separate Unix account (ik-<name>),
# created by `ikenga-server accounts create`, never by this script. Two things
# are shared between them and converged here (founder decisions D-B2, D-B3):
#
#   * ONE download per project: a root-owned, read-only bare mirror in
#     PROJECTS_DIR. Every account has its OWN clone of it (objects shared
#     through git alternates), on its OWN branch. Nobody but root can write
#     the mirror, and no account can touch another's clone, hooks or config.
#   * Scoped secrets: one root-only SECRETS_FILE, each secret tagged for
#     `everyone`, `agents`, or named accounts; each account gets only its own.
#
# A daemon session runs as the account's uid with NO supplementary groups
# (src-tauri/src/executor/t1.rs:3-4 and verify_dropped(), which fails the spawn
# if getgroups() is non-empty). So a group cannot grant a terminal or Chi run
# anything. Read access to a non-world-readable mirror is therefore a READ-ONLY
# POSIX ACL entry per member (matched on the uid, which survives the group
# drop). There is no group and no ACL that grants write anywhere. Every command
# run on an account's behalf below goes through `setpriv --clear-groups`, so it
# sees exactly what the daemon's sessions see.
#
# The summary lists secret NAMES only. Values live in shell variables and are
# written with the printf builtin: never argv, never the log, never `eval`.

SECRETS_DIR="/etc/ikenga/secrets"
SECRETS_BACKUP_DIR="/etc/ikenga/secrets-backup"   # root-only: a backup the account could read would undo a narrowing
SECRETS_LOADER="/etc/profile.d/ikenga-secrets.sh"
GITCONFIG_SYSTEM="/etc/gitconfig"
BASH_BASHRC="/etc/bash.bashrc"
FAILED=0
GIT_HOME=""; ASKPASS_FILE=""; MIRROR_UMASK=077     # git_root's scratch dir, askpass, umask (see sync_projects)
soft_fail() { printf 'warning: %s\n' "$*" >&2; FAILED=1; }

declare -A ACCT_UID=() ACCT_GID=() ACCT_HOME=()
ACCT_LOGINS=()
MEMBERS=()

# Names a secret may not take: they would change how the shell or git behaves,
# or collide with the daemon's own variables.
secret_name_ok() {
  [[ "$1" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || return 1
  case "$1" in
    IKENGA_*|LD_*|DYLD_*|BASH_*|PATH|HOME|USER|LOGNAME|SHELL|IFS|ENV|PS1|PS2|PS4|PROMPT_COMMAND|CDPATH|GLOBIGNORE|SHELLOPTS|TMPDIR|TERM|PWD|OLDPWD) return 1 ;;
    GIT_ASKPASS|GIT_SSH|GIT_SSH_COMMAND|GIT_PROXY_COMMAND|GIT_EXEC_PATH|GIT_DIR|GIT_WORK_TREE|GIT_CONFIG*) return 1 ;;
  esac
}

is_reserved_scope() { [[ "$1" == everyone || "$1" == agents || "$1" == root || "$1" == backup ]]; }

validate_accounts_profile() {
  local a e name rest
  for a in "${ACCOUNTS[@]}" "${AGENT_ACCOUNTS[@]}"; do
    [[ "$a" =~ ^[a-z][a-z0-9_-]{0,30}$ ]] || die "account '$a' is not a valid login name (lowercase letters, digits, - and _; the unix user is ik-<name>)"
    is_reserved_scope "$a" && die "account name '$a' is reserved (everyone, agents, root and backup are secret scopes)"
  done
  [[ "$UID_RANGE" =~ ^[0-9]+-[0-9]+$ ]] || die "UID_RANGE must look like 20000-29999 (got '$UID_RANGE')"
  [[ "$PROJECTS_DIR" =~ ^/[A-Za-z0-9._/-]*[A-Za-z0-9._-]$ && "$PROJECTS_DIR" != *..* ]] || die "PROJECTS_DIR must be an absolute path without spaces or '..' (got '$PROJECTS_DIR')"
  [[ "$PROJECTS_READ" == members || "$PROJECTS_READ" == world ]] || die "PROJECTS_READ must be 'members' or 'world' (got '$PROJECTS_READ')"
  [[ "$PROJECTS_BRANCH_PREFIX" =~ ^[A-Za-z0-9._/-]*$ ]] || die "PROJECTS_BRANCH_PREFIX has characters git branch names should not"
  local seen=" "
  for e in "${PROJECTS[@]}"; do
    name="${e%%=*}"; rest="${e#*=}"
    [[ "$e" == *=* && -n "$rest" ]] || die "PROJECTS entry '$e' must look like name=git-url[#branch]"
    [[ "$name" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$ && "$name" != *.git ]] || die "PROJECTS name '$name' is not a plain directory name"
    [[ "$seen" != *" $name "* ]] || die "PROJECTS names '$name' twice"
    seen+="$name "
    # The URL becomes git's argument: no option look-alikes, and only the
    # transports git_root knows how to restrict. A credential in it would show
    # in argv; private repos use PROJECTS_TOKEN_SECRET instead.
    [[ "${rest%%#*}" =~ ^(https?://|ssh://|file://|/|[A-Za-z0-9._-]+@[A-Za-z0-9._-]+:) ]] \
      || die "PROJECTS '$name': the URL must be https://, http://, ssh://, file://, an absolute path, or user@host:path"
    if [[ "$rest" == *"#"* ]]; then
      [[ "${rest#*#}" =~ ^[A-Za-z0-9][A-Za-z0-9._/-]*$ ]] || die "PROJECTS '$name': the #branch '${rest#*#}' is not a plain branch name"
    fi
    if [[ "${rest%%#*}" =~ ^https?://[^/]*@ ]]; then
      die "PROJECTS '$name': the URL carries credentials; remove them and set PROJECTS_TOKEN_SECRET (a secret in SECRETS_FILE)"
    fi
  done
  [[ -z "$PROJECTS_TOKEN_SECRET" || -n "$SECRETS_FILE" ]] || die "PROJECTS_TOKEN_SECRET needs SECRETS_FILE"
  [[ -z "$PROJECTS_TOKEN_SECRET" ]] || secret_name_ok "$PROJECTS_TOKEN_SECRET" || die "PROJECTS_TOKEN_SECRET '$PROJECTS_TOKEN_SECRET' is not a usable secret name"
  [[ -z "$SECRETS_FILE" || -f "$SECRETS_FILE" ]] || die "SECRETS_FILE '$SECRETS_FILE' does not exist"
}

# ---- who the accounts are

# ACCOUNTS (+ AGENT_ACCOUNTS) are the managed set. With ACCOUNTS empty, every
# ik-* user whose uid lies in UID_RANGE joins it too. A name with no passwd
# entry yet is "pending": it keeps its place in scopes, and is skipped until
# `accounts create` makes it.
resolve_accounts() {
  ACCT_LOGINS=(); ACCT_UID=(); ACCT_GID=(); ACCT_HOME=()
  local seen=" " a line lo hi name uid gid home
  add_login() { [[ "$seen" == *" $1 "* ]] || { seen+="$1 "; ACCT_LOGINS+=("$1"); }; }
  for a in "${ACCOUNTS[@]}" "${AGENT_ACCOUNTS[@]}"; do add_login "$a"; done
  if [[ ${#ACCOUNTS[@]} -eq 0 ]]; then
    lo="${UID_RANGE%-*}"; hi="${UID_RANGE#*-}"
    while IFS=: read -r name _ uid _; do
      [[ "$name" == ik-?* && "$uid" =~ ^[0-9]+$ ]] && (( uid >= lo && uid < hi )) && add_login "${name#ik-}"
    done < <(getent passwd)
  fi
  for a in "${ACCT_LOGINS[@]}"; do
    line="$(getent passwd "ik-$a" || true)"
    if [[ -z "$line" ]]; then
      note "ik-$a: not created yet (accounts create $a); skipped until it exists"
      continue
    fi
    IFS=: read -r _ _ uid gid _ home _ <<<"$line"
    (( uid > 0 )) || die "ik-$a has uid 0"
    ACCT_UID[$a]="$uid"; ACCT_GID[$a]="$gid"; ACCT_HOME[$a]="$home"
  done
  MEMBERS=()
  if [[ ${#PROJECTS_MEMBERS[@]} -eq 0 ]]; then
    MEMBERS=("${ACCT_LOGINS[@]}")
  else
    for a in "${PROJECTS_MEMBERS[@]}"; do
      [[ "$seen" == *" $a "* ]] || die "PROJECTS_MEMBERS names '$a', which is not a managed account (add it to ACCOUNTS)"
      MEMBERS+=("$a")
    done
  fi
  note "managed accounts: ${ACCT_LOGINS[*]:-none}"
}

has_account() { [[ -n "${ACCT_UID[$1]:-}" ]]; }

# Run a command AS an account the way the daemon's sessions run: its uid and
# private gid, no supplementary groups, a minimal environment.
as_account() {
  local a="$1"; shift
  ( cd / && umask 0002 && exec setpriv --reuid="${ACCT_UID[$a]}" --regid="${ACCT_GID[$a]}" --clear-groups \
      env -i HOME="${ACCT_HOME[$a]}" PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 GIT_TERMINAL_PROMPT=0 "$@" )
}

# ---- managed blocks

# managed_block <file> <tag> <top|bottom> <content>
# Keeps one `# ikenga: <tag> begin|end` block in <file>; empty content removes
# it. Returns 0 when it changed the file, 1 when nothing differed. Every change
# to an existing file leaves a .bak-<time> beside it; the last five are kept.
managed_block() {
  local f="$1" tag="$2" pos="$3" content="$4"
  local b="# ikenga: $tag begin" e="# ikenga: $tag end" have=0 cur=""
  if grep -qxF "$b" "$f" 2>/dev/null; then
    have=1
    cur="$(awk -v b="$b" -v e="$e" '$0==b{s=1;next} $0==e{s=0;next} s' "$f")"
  fi
  if [[ -z "$content" ]]; then [[ $have -eq 1 ]] || return 1
  elif [[ $have -eq 1 && "$cur" == "$content" ]]; then return 1
  fi
  [[ $DRY_RUN -eq 1 ]] && return 0
  local tmp; tmp="$(mktemp)"
  { [[ "$pos" == top && -n "$content" ]] && printf '%s\n%s\n%s\n' "$b" "$content" "$e"
    [[ -f "$f" ]] && awk -v b="$b" -v e="$e" '$0==b{s=1;next} $0==e{s=0;next} !s' "$f"
    [[ "$pos" == bottom && -n "$content" ]] && printf '%s\n%s\n%s\n' "$b" "$content" "$e"
    true
  } > "$tmp"
  if [[ -f "$f" ]]; then
    cp -a "$f" "$f.bak-$(date +%Y%m%d-%H%M%S)"
    # Keep the last five backups (their names sort by time).
    find "$(dirname -- "$f")" -maxdepth 1 -name "$(basename -- "$f").bak-*" | sort | head -n -5 | xargs -r rm -f --
    cat "$tmp" > "$f"                      # keeps the file's owner and mode
  else
    install -m 0644 -o root -g root "$tmp" "$f"
  fi
  rm -f "$tmp"
  return 0
}

# ---- shared project mirrors (D-B2)
#
# One read-only bare MIRROR per project, owned by root, that nobody but root
# can write: no group write, no ACL write, nothing a member could plant in it
# (config, hooks/, refs, objects). Each account has its OWN ordinary clone at
# ~/projects/<name>, made as that user with `git clone --reference <mirror>`
# (objects/info/alternates -> the mirror), origin = the mirror path. Its hooks,
# config, refs and index are its own, so one account can never run code as, or
# change the work of, another. Mirrors are never gc'd or pruned: the accounts'
# alternates depend on their objects staying put.
#
# Root's own git never trusts a repo's config or a member's environment: see
# git_root. Nothing root writes lands in a member-writable directory.

# git_root <url|none> <git args...>: git run as root with nothing inherited.
#  - env -i, HOME in a root-only scratch dir, system + global config off, cwd
#    in that scratch dir (never a directory a member can write);
#  - hooks, fsmonitor, credential helpers, redirects, gc and maintenance off;
#  - only the protocol the profile's URL uses is allowed;
#  - the deploy token, for an http(s) URL only, reaches git through GIT_ASKPASS
#    (an askpass script reading a 0600 file in the scratch dir): never argv,
#    never a repo config.
git_root() {
  local url="$1"; shift
  local proto=none
  case "$url" in
    https://*) proto=https ;;
    http://*) proto=http ;;
    ssh://*|[A-Za-z0-9._-]*@*:*) proto=ssh ;;
    file://*|/*) proto=file ;;
  esac
  local -a e=(PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 HOME="$GIT_HOME"
    GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0
    GIT_ALLOW_PROTOCOL="$proto" GIT_SSH_COMMAND="ssh -o BatchMode=yes")
  if [[ -n "$ASKPASS_FILE" && ( "$proto" == https || "$proto" == http ) ]]; then
    e+=(GIT_ASKPASS="$ASKPASS_FILE" IKENGA_TOKEN_FILE="$GIT_HOME/token")
  fi
  ( umask "$MIRROR_UMASK"; cd "$GIT_HOME" && exec env -i "${e[@]}" git \
      -c core.hooksPath=/dev/null -c core.fsmonitor=false -c credential.helper= \
      -c http.followRedirects=false -c gc.auto=0 -c maintenance.auto=false "$@" )
}

# Named-user ACL entries (on the directory itself) as "ik-ada ik-grace".
acl_users() { getfacl -cp -- "$1" 2>/dev/null | sed -n 's/^user:\(ik-[^:]*\):.*/\1/p'; }
acl_has_r() { getfacl -cp -- "$1" 2>/dev/null | grep -q "^user:$2:r-x"; }

# Make a mirror (or PROJECTS_DIR) exactly: root-owned, nobody can write,
# nothing setuid/setgid, no symlinks, no hooks, no foreign alternates. Read
# access: world (PROJECTS_READ=world) or per-member READ-ONLY ACL entries
# (PROJECTS_READ=members; matched on the uid, so they hold for daemon sessions
# that run with no supplementary groups). Reports a change only when it
# actually had to repair something (and not at all with a second argument).
lock_mirror() {
  local repo="$1" quiet="${2:-}" a u want="" fixed=0 bad
  # Debris an older (group-writable) layout, or a member, could have left.
  if [[ -e "$repo/hooks" || -e "$repo/worktrees" || -e "$repo/.fetch.err" \
        || -e "$repo/objects/info/alternates" || -e "$repo/objects/info/http-alternates" ]]; then
    fixed=1
    [[ $DRY_RUN -eq 1 ]] || rm -rf -- "$repo/hooks" "$repo/worktrees" "$repo/.fetch.err" \
      "$repo/objects/info/alternates" "$repo/objects/info/http-alternates"
  fi
  if [[ "$PROJECTS_READ" == world ]]; then
    bad="$(find "$repo" \( ! -user root -o ! -group root -o -type l -o -perm /6022 -o ! -perm -004 -o \( -type d ! -perm -005 \) \) -print -quit 2>/dev/null)"
  else
    bad="$(find "$repo" \( ! -user root -o ! -group root -o -type l -o -perm /6027 -o \( -type d ! -perm -050 \) \) -print -quit 2>/dev/null)"
  fi
  [[ -z "$bad" ]] || fixed=1
  if [[ $DRY_RUN -eq 0 ]]; then
    find "$repo" -type l -delete
    chown -R root:root -- "$repo"
    if [[ "$PROJECTS_READ" == world ]]; then chmod -R u=rwX,go=rX,u-s,g-s -- "$repo"
    else chmod -R u=rwX,g=rX,o=,u-s,g-s -- "$repo"; fi
  fi
  if [[ "$PROJECTS_READ" == world ]]; then
    if [[ -n "$(acl_users "$repo")" ]]; then
      fixed=1; [[ $DRY_RUN -eq 1 ]] || setfacl -R -b -- "$repo"
    fi
  else
    for a in "${MEMBERS[@]}"; do has_account "$a" && want+=" ik-$a"; done
    for u in $want; do
      acl_has_r "$repo" "$u" || fixed=1
      [[ $DRY_RUN -eq 1 ]] || setfacl -R -m "u:$u:rX" -- "$repo" \
        || soft_fail "setfacl failed on $repo: this filesystem may not support ACLs, so daemon sessions (no supplementary groups) cannot read the mirror"
    done
    for u in $(acl_users "$repo"); do
      [[ " $want " == *" $u "* ]] && continue
      fixed=1; [[ $DRY_RUN -eq 1 ]] || setfacl -R -x "u:$u" -- "$repo"
    done
  fi
  [[ $fixed -eq 0 || -n "$quiet" ]] || changed "mirror $repo locked down (root-owned, read-only for everyone else)"
}

# The only config a mirror ever has, written by root. Anything else in an
# existing mirror's config (a legacy shared-group setting, a remote, a planted
# key) is replaced.
write_mirror_config() {
  local repo="$1" tmp="$GIT_HOME/mirror.config"
  printf '%s\n' '[core]' '	repositoryformatversion = 0' '	filemode = true' '	bare = true' \
    '[gc]' '	auto = 0' '[ikenga]' '	mirror = true' > "$tmp"
  if ! cmp -s "$tmp" "$repo/config" 2>/dev/null; then
    [[ $DRY_RUN -eq 1 ]] || install -m 0644 -o root -g root "$tmp" "$repo/config"
    return 0
  fi
  return 1
}

sync_projects() {
  log "Shared project mirrors ($PROJECTS_DIR, read: $PROJECTS_READ)"
  apt_install git acl
  case "$PROJECTS_READ" in world) MIRROR_UMASK=022 ;; *) MIRROR_UMASK=077 ;; esac

  # Root writes under PROJECTS_DIR, so no other user may be able to create or
  # swap anything on the path to it: every existing ancestor must be a real
  # directory, root-owned, not writable by group or others. Refuse rather than
  # adopt a PROJECTS_DIR that is a symlink or not root-owned.
  [[ "$PROJECTS_DIR" == /* && "$PROJECTS_DIR" != *"/../"* && "$PROJECTS_DIR" != *"/.." ]] \
    || die "PROJECTS_DIR must be an absolute path without '..' (got '$PROJECTS_DIR')"
  local anc="$PROJECTS_DIR" ao am
  while :; do
    anc="$(dirname "$anc")"
    if [[ -e "$anc" || -L "$anc" ]]; then
      [[ -L "$anc" ]] && die "PROJECTS_DIR ancestor $anc is a symlink; refusing (another user could redirect root's writes)"
      read -r ao am < <(stat -c '%u %a' -- "$anc")
      [[ "$ao" == 0 ]] || die "PROJECTS_DIR ancestor $anc is not owned by root; refusing"
      (( (8#$am & 8#022) == 0 )) || die "PROJECTS_DIR ancestor $anc is writable by group or others (mode $am); refusing"
    fi
    [[ "$anc" == / ]] && break
  done
  if [[ -L "$PROJECTS_DIR" ]]; then die "PROJECTS_DIR $PROJECTS_DIR is a symlink; refusing"; fi
  if [[ -d "$PROJECTS_DIR" && "$(stat -c '%u' -- "$PROJECTS_DIR")" != 0 ]]; then
    die "PROJECTS_DIR $PROJECTS_DIR exists but is not owned by root; refusing to adopt it"
  fi

  local parent; parent="$(dirname "$PROJECTS_DIR")"
  if [[ ! -d "$PROJECTS_DIR" ]]; then
    if [[ $DRY_RUN -eq 0 ]]; then
      [[ -d "$parent" ]] || install -d -m 0755 -o root -g root "$parent"
      install -d -m 0755 -o root -g root "$PROJECTS_DIR"
    fi
    changed "$PROJECTS_DIR created (root:root 0755: nobody else can add or change a mirror)"
  else
    local o g m; read -r o g m < <(stat -c '%U %G %a' -- "$PROJECTS_DIR")
    if [[ "$o" != root || "$g" != root || "$m" != 755 ]] || getfacl -cp -- "$PROJECTS_DIR" 2>/dev/null | grep -q '^\(default:\)\?user:[^:]'; then
      if [[ $DRY_RUN -eq 0 ]]; then
        chown root:root "$PROJECTS_DIR"; chmod 0755 "$PROJECTS_DIR"; setfacl -b -- "$PROJECTS_DIR"
      fi
      changed "$PROJECTS_DIR reset to root:root 0755 (was $o:$g $m)"
    fi
    [[ $DRY_RUN -eq 1 ]] || rm -rf -- "$PROJECTS_DIR"/.new-*      # an interrupted earlier run
  fi

  # safe.directory for exactly the mirrors: git refuses to fetch or clone from
  # a repo owned by another uid otherwise. Not '*', and not a wildcard: git
  # before 2.46 only understands a lone '*'. (/etc/gitconfig is root-only.)
  local e name block=""
  for e in "${PROJECTS[@]}"; do
    name="${e%%=*}"
    block+="[safe]
	directory = $PROJECTS_DIR/$name.git
"
  done
  if managed_block "$GITCONFIG_SYSTEM" "projects safe.directory" bottom "${block%$'\n'}"; then
    changed "safe.directory for the shared mirrors in $GITCONFIG_SYSTEM"
  fi

  # Root-only scratch dir: git's HOME and cwd, and the deploy token's askpass.
  ASKPASS_FILE=""
  GIT_HOME="$(mktemp -d)"; chmod 0700 "$GIT_HOME"
  trap 'rm -rf -- "${GIT_HOME:-/nonexistent}"' EXIT
  local PROJ_TOKEN=""
  if [[ -n "$PROJECTS_TOKEN_SECRET" ]]; then
    [[ "${ROOT_COUNT[$PROJECTS_TOKEN_SECRET]:-0}" -eq 1 ]] \
      || { [[ $SECRETS_UNREADABLE -eq 1 ]] || die "PROJECTS_TOKEN_SECRET '$PROJECTS_TOKEN_SECRET' must appear exactly once in SECRETS_FILE (found ${ROOT_COUNT[$PROJECTS_TOKEN_SECRET]:-0})"; }
    PROJ_TOKEN="${ROOT_VALUE[$PROJECTS_TOKEN_SECRET]:-}"
    if [[ -n "${ROOT_SCOPE[$PROJECTS_TOKEN_SECRET]:-}" && "${ROOT_SCOPE[$PROJECTS_TOKEN_SECRET]}" != root ]]; then
      note "WARNING: $PROJECTS_TOKEN_SECRET is the deploy token and is scoped '${ROOT_SCOPE[$PROJECTS_TOKEN_SECRET]}', so those accounts get it too. Scope it 'root' to keep it provisioner-only."
    fi
    if [[ -n "$PROJ_TOKEN" && $DRY_RUN -eq 0 ]]; then
      ( umask 077
        printf '%s\n' "$PROJ_TOKEN" > "$GIT_HOME/token"
        printf '%s\n' '#!/bin/sh' 'case "$1" in *sername*) echo x-access-token ;; *) cat "$IKENGA_TOKEN_FILE" ;; esac' > "$GIT_HOME/askpass"
        chmod 0700 "$GIT_HOME/askpass" )
      ASKPASS_FILE="$GIT_HOME/askpass"
    fi
  fi
  PROJ_TOKEN=""

  for e in "${PROJECTS[@]}"; do ensure_mirror "${e%%=*}" "${e#*=}"; done
  local a
  for a in "${MEMBERS[@]}"; do
    has_account "$a" || continue
    for e in "${PROJECTS[@]}"; do ensure_account_clone "$a" "${e%%=*}"; done
  done
  rm -rf -- "$GIT_HOME"; GIT_HOME=""; ASKPASS_FILE=""
}

ensure_mirror() {
  local name="$1" spec="$2" url branch repo work before after def cur first out rc=0 new=0
  url="${spec%%#*}"; branch=""; [[ "$spec" == *"#"* ]] && branch="${spec#*#}"
  repo="$PROJECTS_DIR/$name.git"
  if [[ -L "$repo" ]] || { [[ -e "$repo" ]] && [[ "$(stat -c '%u' -- "$repo")" != 0 ]]; }; then
    soft_fail "project $name: $repo is a symlink or not owned by root; refusing to use it"; return
  fi
  if [[ ! -d "$repo" ]]; then
    if [[ $DRY_RUN -eq 1 ]]; then changed "project $name mirrored from $url"; return; fi
    new=1
    work="$(mktemp -d "$PROJECTS_DIR/.new-$name.XXXXXX")"
    git_root none init --bare -q --template= -- "$work" || { soft_fail "project $name: git init failed"; rm -rf -- "$work"; return; }
    write_mirror_config "$work" || true
  else
    work="$repo"
    if [[ $DRY_RUN -eq 1 ]]; then
      lock_mirror "$repo"
      write_mirror_config "$work" && changed "mirror $name: config reset to the canonical one"
      note "[dry-run] fetch $name from $url"; return
    fi
    # Lock FIRST: on a mirror left group-writable by an older layout, a member
    # could swap in a config between our write and the lock.
    lock_mirror "$repo"
    write_mirror_config "$work" && changed "mirror $name: config reset to the canonical one"
  fi

  # The fetch URL is the PROFILE's. The mirror's own config names no remote
  # and is not consulted; the refspecs make it an exact copy of the remote's
  # branches and tags. --prune drops refs only; no object is ever deleted.
  digest() { git_root none --git-dir "$work" for-each-ref --format='%(objectname) %(refname)' refs/heads refs/tags | sha256sum; }
  before="$(digest)"
  out="$(git_root "$url" --git-dir "$work" fetch --quiet --prune --no-write-fetch-head -- "$url" '+refs/heads/*:refs/heads/*' '+refs/tags/*:refs/tags/*' 2>&1)" || rc=$?
  if [[ $rc -ne 0 ]]; then
    if [[ $new -eq 1 ]]; then
      soft_fail "project $name: cannot fetch $url: $(tr '\n' ' ' <<<"$out" | cut -c1-200)"
      rm -rf -- "$work"; return
    fi
    soft_fail "project $name: fetch from $url failed (mirror left as it was): $(tr '\n' ' ' <<<"$out" | cut -c1-200)"
  fi
  after="$(digest)"

  # Default branch: the profile's #branch, else keep the mirror's HEAD while it
  # resolves, else ask the remote. The name is checked before it is used.
  def="$branch"
  if [[ -z "$def" ]]; then
    cur="$(git_root none --git-dir "$work" symbolic-ref -q --short HEAD || true)"
    if [[ -n "$cur" ]] && git_root none --git-dir "$work" show-ref --verify --quiet "refs/heads/$cur"; then def="$cur"
    else
      def="$(git_root "$url" ls-remote --symref -- "$url" HEAD 2>/dev/null | awk '/^ref:/{sub("refs/heads/","",$2); print $2; exit}')"
      if [[ -z "$def" ]]; then
        first="$(git_root none --git-dir "$work" for-each-ref --format='%(refname:strip=2)' refs/heads | head -1)"
        def="$first"
      fi
    fi
  fi
  if [[ -n "$def" ]] && git_root none check-ref-format --branch "$def" >/dev/null 2>&1; then
    cur="$(git_root none --git-dir "$work" symbolic-ref -q HEAD || true)"
    [[ "$cur" == "refs/heads/$def" ]] || git_root none --git-dir "$work" symbolic-ref HEAD "refs/heads/$def"
  else
    def=""
  fi

  if [[ $new -eq 1 ]]; then
    mv -T -- "$work" "$repo" || { soft_fail "project $name: cannot move the new mirror into place"; rm -rf -- "$work"; return; }
    lock_mirror "$repo" quiet
    changed "project $name mirrored from $url (default branch ${def:-none yet})"
    return
  fi
  lock_mirror "$repo" quiet            # what the fetch just wrote gets the same modes
  [[ "$before" == "$after" ]] || changed "project $name fetched new commits"
}

# An account's own clone: created once, as the account, and then never touched.
ensure_account_clone() {
  local a="$1" name="$2" repo wt branch def home out
  repo="$PROJECTS_DIR/$name.git"; home="${ACCT_HOME[$a]}"; wt="$home/projects/$name"
  branch="${PROJECTS_BRANCH_PREFIX}${a}/main"
  [[ ! -e "$wt" && ! -L "$wt" ]] || return 0           # never touch an existing clone
  if [[ $DRY_RUN -eq 1 ]]; then
    [[ -d "$repo" ]] && changed "clone ik-$a:$name on $branch" || changed "clone ik-$a:$name on $branch (after the mirror)"
    return
  fi
  [[ -d "$repo" ]] || return 0
  def="$(git_root none --git-dir "$repo" symbolic-ref -q --short HEAD || true)"
  if [[ -z "$def" ]] || ! git_root none --git-dir "$repo" show-ref --verify --quiet "refs/heads/$def"; then
    note "ik-$a:$name: the mirror has no commits on '${def:-?}' yet; no clone"
    return
  fi
  [[ "$(stat -c %u -- "$home" 2>/dev/null)" == "${ACCT_UID[$a]}" ]] || { soft_fail "ik-$a: home $home is not owned by the account; no clone"; return; }
  as_account "$a" mkdir -p -- "$home/projects" || { soft_fail "ik-$a: cannot create $home/projects"; return; }
  # --no-local: transfer through upload-pack, never hard-link or copy from the
  # root-owned mirror. --reference: the account's objects/info/alternates point
  # at the mirror, so the project is stored once.
  if ! out="$(as_account "$a" git clone --quiet --no-local --no-checkout --reference "$repo" -- "$repo" "$wt" 2>&1)"; then
    soft_fail "ik-$a:$name: git clone failed: $(tr '\n' ' ' <<<"$out" | cut -c1-200)"; return
  fi
  if ! out="$(as_account "$a" git -C "$wt" checkout --quiet --no-track -b "$branch" "refs/remotes/origin/$def" 2>&1)"; then
    soft_fail "ik-$a:$name: cannot create $branch: $(tr '\n' ' ' <<<"$out" | cut -c1-200)"; return
  fi
  changed "clone ik-$a:$name on $branch"
}

# ---- scoped secrets (D-B3)
#
# SECRETS_FILE, one secret per line, the scope in front so the value is
# everything after the first '=' and is never parsed:
#
#   [everyone]       NAME=value
#   [agents]         NAME=value        # the accounts in AGENT_ACCOUNTS
#   [rex]            NAME=value        # one account (login name, not ik-rex)
#   [ada,grace]      NAME=value        # several
#   [root]           NAME=value        # provisioner only, delivered to nobody
#
# Blank lines and lines starting with # are skipped. A secret with no scope is
# an error: there is no default audience.

declare -A ROOT_VALUE=() ROOT_COUNT=() ROOT_SCOPE=() SEC_BODY=() SEC_SEEN=() BACKUP_VALUE=()
SECRETS_UNREADABLE=0

secrets_file_ok() {
  local f="$1" owner mode dir dmode
  [[ -f "$f" && ! -L "$f" ]] || die "SECRETS_FILE $f must be a regular file, not a symlink"
  read -r owner mode < <(stat -c '%u %a' -- "$f")
  [[ "$owner" == 0 ]] || die "SECRETS_FILE $f must be owned by root"
  (( (8#$mode & 8#077) == 0 )) || die "SECRETS_FILE $f has mode $mode; it must be 0600 (root-only). Refusing to read it. Fix: chmod 600 $f"
  dir="$(dirname -- "$f")"
  read -r owner dmode < <(stat -c '%u %a' -- "$dir")
  [[ "$owner" == 0 ]] && (( (8#$dmode & 8#022) == 0 )) \
    || die "the directory $dir holding SECRETS_FILE must be owned by root and not writable by anyone else"
}

load_secrets() {
  SEC_BODY=(); SEC_SEEN=(); ROOT_VALUE=(); ROOT_COUNT=(); ROOT_SCOPE=(); BACKUP_VALUE=(); SECRETS_UNREADABLE=0
  [[ -n "$SECRETS_FILE" ]] || return 0
  if [[ ! -r "$SECRETS_FILE" ]]; then
    [[ $DRY_RUN -eq 1 ]] || die "cannot read SECRETS_FILE $SECRETS_FILE"
    SECRETS_UNREADABLE=1; note "WARNING: $SECRETS_FILE is not readable by this user; the secrets plan is skipped (run the dry run as root)"
    return 0
  fi
  secrets_file_ok "$SECRETS_FILE"
  local re='^\[([^]]+)\][[:space:]]+([A-Za-z_][A-Za-z0-9_]*)=(.*)$'
  local n=0 line scope name value tok a toks targets declared is_backup
  while IFS= read -r line || [[ -n "$line" ]]; do
    n=$((n+1))
    [[ "$line" =~ ^[[:space:]]*(#.*)?$ ]] && continue
    # Never echo the line: it holds the value.
    [[ "$line" =~ $re ]] || die "$SECRETS_FILE line $n: expected '[scope] NAME=value'"
    scope="${BASH_REMATCH[1]}"; name="${BASH_REMATCH[2]}"; value="${BASH_REMATCH[3]}"
    secret_name_ok "$name" || die "$SECRETS_FILE line $n: '$name' is not an allowed secret name (reserved or shell-sensitive)"
    [[ -n "$value" ]] || die "$SECRETS_FILE line $n: $name has an empty value"
    [[ "$value" != *$'\r'* ]] || die "$SECRETS_FILE line $n: carriage return in the value of $name (CRLF file?)"

    targets=" "; declared=" "; is_backup=0
    IFS=',' read -ra toks <<<"$scope"
    for tok in "${toks[@]}"; do
      tok="${tok//[[:space:]]/}"
      [[ -n "$tok" ]] || die "$SECRETS_FILE line $n: empty scope entry"
      [[ "$declared" != *" $tok "* ]] || die "$SECRETS_FILE line $n: scope names '$tok' twice"
      declared+="$tok "
      case "$tok" in
        everyone) for a in "${ACCT_LOGINS[@]}"; do [[ "$targets" == *" $a "* ]] || targets+="$a "; done ;;
        agents)
          [[ ${#AGENT_ACCOUNTS[@]} -gt 0 ]] || die "$SECRETS_FILE line $n: scope 'agents' but AGENT_ACCOUNTS is empty"
          for a in "${AGENT_ACCOUNTS[@]}"; do [[ "$targets" == *" $a "* ]] || targets+="$a "; done ;;
        root) ;;
        backup) is_backup=1 ;;
        *)
          [[ " ${ACCT_LOGINS[*]} " == *" $tok "* ]] || die "$SECRETS_FILE line $n: scope names '$tok', which is not a managed account (typo? or add it to ACCOUNTS)"
          [[ "$targets" == *" $tok "* ]] || targets+="$tok " ;;
      esac
    done
    if [[ $is_backup -eq 1 ]]; then
      # Scope `backup` is the dedicated backup user's and nobody else's: it may
      # not be combined with any other scope, so no Ikenga account can get it.
      [[ ${#toks[@]} -eq 1 ]] || die "$SECRETS_FILE line $n: scope 'backup' cannot be combined with other scopes ($name)"
      [[ -z "${BACKUP_VALUE[$name]+x}" ]] || die "$SECRETS_FILE line $n: $name is already set for the backup user by an earlier line"
      BACKUP_VALUE[$name]="$value"
    fi
    ROOT_VALUE[$name]="$value"; ROOT_COUNT[$name]=$(( ${ROOT_COUNT[$name]:-0} + 1 )); ROOT_SCOPE[$name]="$scope"
    for a in $targets; do
      [[ -z "${SEC_SEEN[$a|$name]:-}" ]] || die "$SECRETS_FILE line $n: $name is already set for $a by an earlier line (overlapping scopes)"
      SEC_SEEN[$a|$name]=1
      SEC_BODY[$a]+="$name=$value"$'\n'
    done
  done < "$SECRETS_FILE"
}

# Parse "NAME=value" lines of $1 into the global assoc named by $2.
parse_kv() {
  local -n _kv="$2"; _kv=()
  local l
  while IFS= read -r l; do
    [[ -z "$l" || "$l" == '#'* ]] && continue
    _kv["${l%%=*}"]="${l#*=}"
  done <<<"$1"
}
declare -A OLD_KV=() NEW_KV=()

backup_secret_file() {
  local f="$1" base; base="$(basename -- "$f")"
  install -d -m 0700 -o root -g root "$SECRETS_BACKUP_DIR"
  install -m 0600 -o root -g root "$f" "$SECRETS_BACKUP_DIR/$base.bak-$(date +%Y%m%d-%H%M%S).$$"
  # Keep the last five per account.
  local old; old="$(ls -1t "$SECRETS_BACKUP_DIR/$base".bak-* 2>/dev/null | tail -n +6 || true)"
  [[ -z "$old" ]] || printf '%s\n' "$old" | xargs -r rm -f --
}

LOADER_BODY='# ikenga: managed by provision.sh. Exports the secrets the provisioner granted this
# account, from a root-owned file only this account can read. Parsed line by
# line and exported with `export "NAME=value"`: never evaluated.
_ik_f="/etc/ikenga/secrets/$(id -un 2>/dev/null).env"
if [ -r "$_ik_f" ]; then
  while IFS= read -r _ik_l || [ -n "$_ik_l" ]; do
    case "$_ik_l" in ""|"#"*) continue ;; esac
    case "$_ik_l" in *=*) ;; *) continue ;; esac
    _ik_n=${_ik_l%%=*}; _ik_v=${_ik_l#*=}
    case "$_ik_n" in ""|[0-9]*|*[!A-Za-z0-9_]*) continue ;; esac
    export "$_ik_n=$_ik_v"
  done < "$_ik_f"
fi
unset _ik_f _ik_l _ik_n _ik_v'

sync_secrets() {
  log "Scoped secrets ($SECRETS_DIR)"
  local a f gid desired cur k line

  # Delivery: a root-owned 0640 file per account (group = the account's own
  # private group), loaded by shells. The daemon has no per-principal secret
  # injection from the root side yet, so this reaches login shells and
  # interactive bash only (README "Where the secrets reach").
  if [[ ! -d "$SECRETS_DIR" ]]; then
    if [[ $DRY_RUN -eq 0 ]]; then
      install -d -m 0755 -o root -g root "$(dirname "$SECRETS_DIR")"
      install -d -m 0711 -o root -g root "$SECRETS_DIR"
    fi
    changed "$SECRETS_DIR created"
  fi
  if [[ "$(cat "$SECRETS_LOADER" 2>/dev/null || true)" != "$LOADER_BODY" ]]; then
    if [[ $DRY_RUN -eq 0 ]]; then
      [[ -f "$SECRETS_LOADER" ]] && cp -a "$SECRETS_LOADER" "$SECRETS_LOADER.bak-$(date +%Y%m%d-%H%M%S)"
      printf '%s\n' "$LOADER_BODY" > "$SECRETS_LOADER"; chmod 0644 "$SECRETS_LOADER"
    fi
    changed "$SECRETS_LOADER installed"
  fi
  if managed_block "$BASH_BASHRC" "secrets loader" top "[ -r $SECRETS_LOADER ] && . $SECRETS_LOADER"; then
    changed "secrets loader hooked into $BASH_BASHRC"
  fi

  if [[ $SECRETS_UNREADABLE -eq 1 ]]; then note "secrets plan skipped (SECRETS_FILE not readable)"; return; fi

  for a in "${ACCT_LOGINS[@]}"; do
    has_account "$a" || continue
    f="$SECRETS_DIR/ik-$a.env"; gid="${ACCT_GID[$a]}"
    desired="${SEC_BODY[$a]:-}"
    cur=""; [[ ! -f "$f" ]] || cur="$(cat -- "$f" 2>/dev/null || true)"
    parse_kv "$cur" OLD_KV
    parse_kv "$desired" NEW_KV
    local diff=""
    for k in $(printf '%s\n' "${!NEW_KV[@]}" | sort); do
      if [[ -z "${OLD_KV[$k]+x}" ]]; then diff+=" +$k"
      elif [[ "${OLD_KV[$k]}" != "${NEW_KV[$k]}" ]]; then diff+=" ~$k"; fi
    done
    for k in $(printf '%s\n' "${!OLD_KV[@]}" | sort); do
      [[ -n "${NEW_KV[$k]+x}" ]] || diff+=" -$k"
    done

    if [[ -z "$desired" ]]; then
      [[ -f "$f" ]] || continue
      [[ $DRY_RUN -eq 1 ]] || { backup_secret_file "$f"; rm -f -- "$f"; }
      changed "secrets ik-$a:${diff:- (file removed)}"
      continue
    fi
    if [[ -n "$diff" ]]; then
      if [[ $DRY_RUN -eq 0 ]]; then
        [[ -f "$f" ]] && backup_secret_file "$f"
        local tmp; tmp="$(mktemp "$SECRETS_DIR/.tmp.XXXXXX")"
        { printf '%s\n' "# ikenga: managed by provision.sh from SECRETS_FILE; edits are overwritten"; printf '%s' "$desired"; } > "$tmp"
        chown "0:$gid" "$tmp"; chmod 0640 "$tmp"; mv -f -- "$tmp" "$f"
      fi
      changed "secrets ik-$a:$diff"
    else
      local o g m; read -r o g m < <(stat -c '%u %g %a' -- "$f")
      if [[ "$o" != 0 || "$g" != "$gid" || "$m" != 640 ]]; then
        run chown "0:$gid" "$f"; run chmod 0640 "$f"; changed "secrets ik-$a: file owner/mode restored"
      fi
    fi
  done

  # Files for accounts that are no longer managed (or no longer exist).
  if [[ -d "$SECRETS_DIR" ]]; then
    for f in "$SECRETS_DIR"/ik-*.env; do
      [[ -e "$f" ]] || continue
      line="$(basename -- "$f" .env)"; a="${line#ik-}"
      if [[ " ${ACCT_LOGINS[*]} " == *" $a "* ]] && has_account "$a"; then continue; fi
      [[ $DRY_RUN -eq 1 ]] || { backup_secret_file "$f"; rm -f -- "$f"; }
      changed "secrets ik-$a: file removed (account no longer managed)"
    done
  fi
}

sync_accounts() {
  if [[ ${#PROJECTS[@]} -eq 0 && -z "$SECRETS_FILE" ]]; then
    [[ "$ACTION" != sync-accounts ]] || note "nothing to do: the profile sets no PROJECTS and no SECRETS_FILE"
    return 0
  fi
  log "Accounts: shared projects and scoped secrets"
  [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo)"
  resolve_accounts
  load_secrets
  [[ ${#PROJECTS[@]} -eq 0 ]] || sync_projects
  [[ -z "$SECRETS_FILE" ]] || sync_secrets
}

# ------------------------------------------------- database backups (D-B10)
#
# Postgres backups run on THIS box as system jobs, by a dedicated user that is
# not an Ikenga principal and holds only what the jobs need: the connection
# strings and the GCS service-account key. Rex's alerts only read the status
# file the jobs leave behind. Layout (all paths fixed; the unit files name them):
#
#   /usr/local/lib/ikenga-backup/   run-backup.sh, verify-backup.sh   root:root 0755
#   /etc/ikenga-backup/             backup-config.json (names only)    root:<group> 0750 / 0640
#                                   connections.env    (secrets)       root:<group> 0640
#   /var/lib/ikenga-backup/         root:root 0755. Root's, and ONLY root's: the
#                                   backup user cannot create, rename or replace
#                                   anything directly in it.
#     status.json                   symlink -> status/status.json (root-made)
#     status/                       status.json + its lock (everyone reads)  <user> 0755
#     private/                      the user's home: key, gcloud config
#                                   (gcloud/<schedule>), scratch space, raw
#                                   tool errors                              <user> 0700
#
# Root never follows or writes through a path the backup user controls: the two
# directories the user owns (status/, private/) hang off a root-owned parent, so
# they cannot be swapped for symlinks; everything root does INSIDE them (reading
# the status, installing or comparing the key, removing credentials, marking the
# status disabled) is done AS the backup user (as_backup); and a symlink found at
# any of those paths is refused, never adopted.
#
# The connection strings come from SECRETS_FILE lines scoped `backup`; that
# scope can't be combined with another and is never delivered to an account.
# Values are handled like sync_secrets does: shell variables and the printf
# builtin, never argv, never the log, never `eval`.

BACKUP_LIB_DIR="/usr/local/lib/ikenga-backup"
BACKUP_ETC_DIR="/etc/ikenga-backup"
BACKUP_STATE_DIR="/var/lib/ikenga-backup"
BACKUP_STATUS_DIR="$BACKUP_STATE_DIR/status"
BACKUP_PRIV_DIR="$BACKUP_STATE_DIR/private"
BACKUP_GCLOUD_DIR="$BACKUP_PRIV_DIR/gcloud"
BACKUP_CONFIG_DST="$BACKUP_ETC_DIR/backup-config.json"
BACKUP_ENV_DST="$BACKUP_ETC_DIR/connections.env"
BACKUP_KEY_DST="$BACKUP_PRIV_DIR/gcs-key.json"
BACKUP_UNIT="ikenga-backup@.service"
# The PGDG archive signing key (apt.postgresql.org). The download is checked
# against this fingerprint and nothing else is accepted.
PGDG_KEY_FPR="B97B0AFCAA1A47F044F244A07FCC7D46ACCC4CF8"
PGDG_KEY_URL="https://www.postgresql.org/media/keys/ACCC4CF8.asc"
# The Google Cloud apt signing key ("Artifact Registry Repository Signer"),
# observed from packages.cloud.google.com on 2026-10-08 and pinned here. If
# Google rotates it provisioning stops with the new fingerprint named: confirm
# it against Google's install docs, then add it to BACKUP_GCLOUD_KEY_FPRS.
GCLOUD_KEY_FPR="35BAA0B33E9EB396F59CA838C0BA5CE6DC6315A3"
GCLOUD_KEY_URL="https://packages.cloud.google.com/apt/doc/apt-key.gpg"

declare -A BK_CAL=([4hourly]="*-*-* 00/4:00:00 UTC" [6hourly]="*-*-* 00/6:00:00 UTC" [12hourly]="*-*-* 00/12:00:00 UTC"
                   [daily]="*-*-* 02:00:00 UTC" [weekly]="Sun *-*-* 03:00:00 UTC" [monthly]="*-*-01 04:00:00 UTC")
BK_NAMES=(); declare -A BK_SECRET=() BK_SCHED=()
BK_SCHEDS_USED=()
BK_UID=""; BK_GID=""; BK_GROUP=""
BK_NEED_SYSTEMD_RELOAD=0

# Cheap checks that need nothing installed. The deep check of BACKUP_CONFIG is
# backup_load_config, which needs jq and so runs after the packages.
validate_backups_profile() {
  [[ "$BACKUPS_ENABLED" == 0 || "$BACKUPS_ENABLED" == 1 ]] || die "BACKUPS_ENABLED must be 0 or 1 (got '$BACKUPS_ENABLED')"
  [[ "$BACKUP_USER" =~ ^[a-z_][a-z0-9_-]{0,30}$ ]] || die "BACKUP_USER '$BACKUP_USER' is not a valid unix user name"
  case "$BACKUP_USER" in
    root|ikenga|nobody|postgres|"$ADMIN_USER"|ik-*) die "BACKUP_USER '$BACKUP_USER' is not allowed: it must be its own plain system user (not root, the admin, the T0 'ikenga' user, or an ik-* Ikenga account)" ;;
  esac
  [[ "$BACKUP_PG_MAJOR" =~ ^[0-9]{2}$ ]] || die "BACKUP_PG_MAJOR must be two digits, e.g. 17 (got '$BACKUP_PG_MAJOR')"
  # The servers are PostgreSQL 17 and a pg_dump older than its server refuses to
  # dump it, so every run would end in dump-failed: refuse that at provision time.
  (( 10#$BACKUP_PG_MAJOR >= 17 )) || die "BACKUP_PG_MAJOR $BACKUP_PG_MAJOR is older than the servers (PostgreSQL 17): pg_dump refuses to dump a newer server, so every backup would fail. Use 17 or newer"
  [[ "$BACKUP_TIMEOUT_SEC" =~ ^[0-9]{3,6}$ ]] || die "BACKUP_TIMEOUT_SEC must be a number of seconds (got '$BACKUP_TIMEOUT_SEC')"
  local e name cal
  for e in "${BACKUP_SCHEDULES[@]}"; do
    name="${e%%=*}"; cal="${e#*=}"
    [[ "$e" == *=* && -n "$cal" ]] || die "BACKUP_SCHEDULES entry '$e' must look like name=OnCalendar-expression"
    [[ "$name" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]] || die "BACKUP_SCHEDULES name '$name' must be lowercase letters, digits and -"
    # The value lands in a unit file: only what a calendar expression needs.
    [[ "$cal" =~ ^[A-Za-z0-9*/:,.~\ -]+$ ]] || die "BACKUP_SCHEDULES '$name': the calendar expression has characters a systemd OnCalendar does not need"
    if command -v systemd-analyze >/dev/null 2>&1; then
      systemd-analyze calendar "$cal" >/dev/null 2>&1 || die "BACKUP_SCHEDULES '$name': systemd cannot parse the calendar expression '$cal' (try: systemd-analyze calendar '$cal')"
    fi
    BK_CAL[$name]="$cal"
  done
  for e in "${BACKUP_GCLOUD_KEY_FPRS[@]}"; do
    [[ "$e" =~ ^[0-9A-Fa-f]{40}$ ]] || die "BACKUP_GCLOUD_KEY_FPRS entry '$e' is not a 40-hex-digit fingerprint"
  done
  [[ "$BACKUPS_ENABLED" == 1 ]] || return 0
  [[ -n "$BACKUP_CONFIG" && -f "$BACKUP_CONFIG" ]] || die "BACKUPS_ENABLED needs BACKUP_CONFIG: a JSON file of databases (see scripts/server/backup/backup-config.example.json)"
  [[ -n "$SECRETS_FILE" ]] || die "BACKUPS_ENABLED needs SECRETS_FILE (the connection strings are '[backup]' lines in it)"
  [[ -n "$BACKUP_GCS_KEY_SECRET" ]] || die "BACKUPS_ENABLED needs BACKUP_GCS_KEY_SECRET: the NAME of the SECRETS_FILE entry holding the base64 service-account key"
  secret_name_ok "$BACKUP_GCS_KEY_SECRET" || die "BACKUP_GCS_KEY_SECRET '$BACKUP_GCS_KEY_SECRET' is not a usable secret name"
}

backups_installed() {
  [[ -e "$SYSTEMD_DIR/$BACKUP_UNIT" ]] || compgen -G "$SYSTEMD_DIR/ikenga-backup-*.timer" >/dev/null
}

# systemctl, quietly absent on a host without it (dry runs).
sc() { command -v systemctl >/dev/null 2>&1 || return 0; systemctl "$@"; }

# ensure_dir <mode> <owner> <group> <path>
ensure_dir() {
  local mode="$1" owner="$2" group="$3" d="$4" cur
  if [[ -d "$d" ]]; then
    cur="$(stat -c '%a %U %G' -- "$d")"
    [[ "$cur" == "${mode#0} $owner $group" ]] && return 0
    run chown "$owner:$group" "$d"; run chmod "$mode" "$d"
    changed "$d: owner/mode set to $owner:$group $mode"
  else
    run install -d -m "$mode" -o "$owner" -g "$group" "$d"
    changed "$d created ($owner:$group $mode)"
  fi
}

# install_file <mode> <owner> <group> <src> <dst>: copy when the content or
# the owner/mode differ. Returns 0 when it changed something.
install_file() {
  local mode="$1" owner="$2" group="$3" src="$4" dst="$5" cur
  if [[ -f "$dst" ]] && cmp -s -- "$src" "$dst"; then
    cur="$(stat -c '%a %U %G' -- "$dst")"
    [[ "$cur" == "${mode#0} $owner $group" ]] && return 1
    run chown "$owner:$group" "$dst"; run chmod "$mode" "$dst"
    changed "$dst: owner/mode set to $owner:$group $mode"
    return 0
  fi
  run install -m "$mode" -o "$owner" -g "$group" "$src" "$dst"
  changed "$dst installed"
  return 0
}

APT_UPDATED=0
backup_apt_update() {
  [[ $APT_UPDATED -eq 1 ]] && return 0
  run env DEBIAN_FRONTEND=noninteractive apt-get update -y; APT_UPDATED=1
}
backup_apt_install() {
  local missing=() p
  for p in "$@"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
  [[ ${#missing[@]} -gt 0 ]] || return 0
  backup_apt_update
  apt_install "${missing[@]}"
}

# The fingerprints of the primary keys in a key file (armored or binary), one
# per line. Uses a throwaway GNUPGHOME so root's keyring is never touched.
key_fingerprints() {
  local f="$1" home; home="$(mktemp -d)"; chmod 0700 "$home"
  GNUPGHOME="$home" gpg --batch --quiet --show-keys --with-colons --fingerprint -- "$f" 2>/dev/null \
    | awk -F: '$1=="pub"{p=1} $1=="fpr"&&p{print toupper($10); p=0}'
  rm -rf -- "$home"
}

# backup_apt_repo <name> <key url> <allowed fingerprints, space separated> <deb line, with {KEYRING}>
# Adds an apt repository whose signing key is downloaded over https, checked
# against the pinned fingerprints (EVERY key in the file must be one of them),
# and used only through signed-by for this one repository.
backup_apt_repo() {
  local name="$1" url="$2" allowed="$3" line="$4"
  local keyring="/usr/share/keyrings/ikenga-$name.gpg" list="/etc/apt/sources.list.d/ikenga-$name.list"
  local want="${line//\{KEYRING\}/$keyring}"
  if [[ -f "$keyring" && "$(cat "$list" 2>/dev/null || true)" == "$want" ]]; then
    # Already set up: re-check the installed key, so a swapped keyring is noticed.
    local f ok=1
    for f in $(key_fingerprints "$keyring"); do [[ " $allowed " == *" $f "* ]] || ok=0; done
    [[ $ok -eq 1 ]] || die "the installed $keyring holds a key that is not in the pinned set ($allowed); remove it and the $list to re-fetch"
    return 0
  fi
  if [[ $DRY_RUN -eq 1 ]]; then
    changed "apt repository $name ($url, key pinned to $allowed)"
    note "[dry-run] fetch $url, require fingerprint(s) $allowed, install $keyring and $list, apt-get update"
    return 0
  fi
  local tmp key fps f; tmp="$(mktemp -d)"; chmod 0700 "$tmp"
  curl -fsSL --proto '=https' --tlsv1.2 --max-time 60 -o "$tmp/key" "$url" || { rm -rf "$tmp"; die "could not download the $name apt signing key from $url"; }
  fps="$(key_fingerprints "$tmp/key")"
  [[ -n "$fps" ]] || { rm -rf "$tmp"; die "the $name key download is not a PGP key"; }
  for f in $fps; do
    [[ " $allowed " == *" $f "* ]] || { rm -rf "$tmp"; die "the $name apt signing key has fingerprint $f, which is not one of the pinned ones ($allowed); refusing to trust it. If the vendor rotated it, confirm the new fingerprint with them and extend the pin"; }
  done
  # apt wants a binary keyring for signed-by.
  if head -c 64 "$tmp/key" | grep -q 'BEGIN PGP'; then
    gpg --batch --yes --dearmor -o "$tmp/key.gpg" "$tmp/key" 2>/dev/null || { rm -rf "$tmp"; die "could not dearmor the $name key"; }
  else
    cp "$tmp/key" "$tmp/key.gpg"
  fi
  install -m 0644 -o root -g root "$tmp/key.gpg" "$keyring"
  printf '%s\n' "$want" > "$list"; chmod 0644 "$list"
  rm -rf "$tmp"
  changed "apt repository $name added (key $fps)"
  APT_UPDATED=0; backup_apt_update
}

# ---- the config

# Reads and validates BACKUP_CONFIG. Needs jq. Fills BK_NAMES and the
# BK_SECRET/BK_SCHED maps, and BK_SCHEDS_USED.
backup_load_config() {
  BK_NAMES=(); BK_SECRET=(); BK_SCHED=(); BK_SCHEDS_USED=()
  if ! command -v jq >/dev/null 2>&1; then
    [[ $DRY_RUN -eq 1 ]] || die "jq is required to read BACKUP_CONFIG"
    note "jq is not installed here: BACKUP_CONFIG is not validated in this dry run"; return 1
  fi
  jq -e '(.databases | type) == "array"' "$BACKUP_CONFIG" >/dev/null 2>&1 || die "BACKUP_CONFIG $BACKUP_CONFIG is not JSON with a top-level \"databases\" array"
  local errs
  errs="$(jq -r '
    def isstr(re): type == "string" and test(re);
    ( (.databases | to_entries[]) as $e
      | "entry \($e.key + 1)" as $w | $e.value as $d
      | if ($d | type) != "object" then "\($w): not an object"
        else
          ( if ($d.name | isstr("^[A-Za-z0-9][A-Za-z0-9._-]{0,62}$")) then empty else "\($w): name must be letters, digits, . _ - (max 63)" end ),
          ( if ($d.connection_secret | isstr("^[A-Za-z_][A-Za-z0-9_]*$")) then empty else "\($w): connection_secret must be a secret NAME like MY_DB_CONNECTION_STRING" end ),
          ( if ($d.schedule | isstr("^[a-z0-9][a-z0-9-]{0,31}$")) then empty else "\($w): schedule must be a name like daily" end ),
          ( if ($d.gcs_bucket | isstr("^[a-z0-9][a-z0-9._-]{1,220}$")) then empty else "\($w): gcs_bucket is not a bucket name" end ),
          ( if ($d | has("enabled")) and (($d.enabled | type) != "boolean") then "\($w): enabled must be true or false" else empty end ),
          ( if ($d | has("require_auth")) and ($d.require_auth != false)
               and (($d.require_auth | type) != "string"
                    or ($d.require_auth | test("^!?(password|md5|gss|sspi|scram-sha-256|none)(,!?(password|md5|gss|sspi|scram-sha-256|none))*$") | not)
                    or ($d.require_auth | split(",") | map(startswith("!")) | unique | length) > 1)
             then "\($w): require_auth must be false (opt out) or a libpq list such as scram-sha-256 (all negated with ! or none of them)" else empty end )
        end ),
    ( [.databases[] | select(type == "object") | .name] as $n
      | if ($n | length) != ($n | unique | length) then "duplicate database name" else empty end )
  ' "$BACKUP_CONFIG" 2>&1 | sort -u)" || true
  [[ -z "$errs" ]] || die "BACKUP_CONFIG is invalid:"$'\n'"$(printf '  %s\n' "$errs")"
  local name secret sched seen=" "
  while IFS=$'\t' read -r name secret sched; do
    secret_name_ok "$secret" || die "BACKUP_CONFIG: database '$name' uses connection_secret '$secret', which is not an allowed secret name (reserved or shell-sensitive)"
    [[ -n "${BK_CAL[$sched]:-}" ]] || die "BACKUP_CONFIG: database '$name' uses schedule '$sched', which has no calendar (defaults: ${!BK_CAL[*]}; add one with BACKUP_SCHEDULES=(\"$sched=<OnCalendar>\"))"
    BK_NAMES+=("$name"); BK_SECRET[$name]="$secret"; BK_SCHED[$name]="$sched"
    [[ "$seen" == *" $sched "* ]] || { seen+="$sched "; BK_SCHEDS_USED+=("$sched"); }
  done < <(jq -r '.databases[] | select(.enabled == true) | [.name, .connection_secret, .schedule] | @tsv' "$BACKUP_CONFIG")
  [[ ${#BK_NAMES[@]} -gt 0 ]] || die "BACKUP_CONFIG has no enabled database (a database is backed up only with \"enabled\": true)"
  # A database without "enabled": true is skipped, as on rex-vps. Say so by name, so
  # a forgotten field is visible rather than a silent gap in the backups.
  local skipped; skipped="$(jq -r '[.databases[] | select(.enabled != true) | .name] | join(" ")' "$BACKUP_CONFIG")"
  [[ -z "$skipped" ]] || note "databases not backed up (\"enabled\" is not true): $skipped"
  return 0
}

# The local ports of the SSH tunnels on this box, one per line. The profile's
# TUNNELS when it defines them; otherwise (a profile that never mentions tunnels
# leaves the host's alone) whatever the tunnel user's units forward.
backup_tunnel_ports() {
  local n f
  if [[ $TUNNELS_DEFINED -eq 1 ]]; then
    for n in "${TN_NAMES[@]}"; do printf '%s\n' "${TN_LPORT[$n]}"; done
    return 0
  fi
  for f in "$SYSTEMD_DIR"/*-tunnel.service; do
    [[ -f "$f" && ! -L "$f" ]] || continue
    grep -qFx "User=$TUNNEL_USER" "$f" 2>/dev/null || continue
    grep -oE -- '-L 127\.0\.0\.1:[0-9]+:' "$f" 2>/dev/null | cut -d: -f2 || true
  done
}

# Prints the path of the config to install. That is BACKUP_CONFIG itself, unless
# this box has tunnels: then a temporary copy with "tunnel_ports": [..] added.
# run-backup.sh uses it to default require_auth=scram-sha-256 for a database whose
# connection string points at a tunnel's local end (the connection secrets are
# never read or rewritten here, only the names-only config).
backup_render_config() {
  local ports out
  ports="$(backup_tunnel_ports | LC_ALL=C sort -un | jq -Rsc 'split("\n") | map(select(length > 0) | tonumber)')" || ports='[]'
  if [[ "$ports" != '[]' ]]; then
    out="$(mktemp)"
    jq --argjson p "$ports" '.tunnel_ports = $p' "$BACKUP_CONFIG" > "$out" || { rm -f -- "$out"; die "could not add tunnel_ports to a copy of BACKUP_CONFIG"; }
    printf '%s' "$out"
  elif jq -e 'has("tunnel_ports")' "$BACKUP_CONFIG" >/dev/null 2>&1; then
    out="$(mktemp)"
    jq 'del(.tunnel_ports)' "$BACKUP_CONFIG" > "$out" || { rm -f -- "$out"; die "could not drop tunnel_ports from a copy of BACKUP_CONFIG"; }
    printf '%s' "$out"
  else
    printf '%s' "$BACKUP_CONFIG"
  fi
}

# ---- the secrets plan (pure: reads SECRETS_FILE state, writes nothing)

BK_ENV_BODY=""; BK_KEY_JSON=""; BK_MISSING=()
backup_plan_secrets() {
  BK_ENV_BODY=""; BK_KEY_JSON=""; BK_MISSING=()
  [[ $SECRETS_UNREADABLE -eq 0 ]] || return 1
  local name db a scope used=" " k line b64
  # Connection strings: scope `backup` only, one line each.
  for db in "${BK_NAMES[@]}"; do
    name="${BK_SECRET[$db]}"
    if [[ -z "${BACKUP_VALUE[$name]+x}" ]]; then
      if [[ -n "${ROOT_SCOPE[$name]:-}" ]]; then
        die "the connection secret $name (database $db) is in SECRETS_FILE with scope [${ROOT_SCOPE[$name]}], not [backup]. Only the backup user may hold it: change that line's scope to [backup]"
      fi
      BK_MISSING+=("$db:$name"); continue
    fi
    [[ "$used" == *" $name "* ]] && continue
    used+="$name "
  done
  # The key: scope root or backup, exactly once.
  name="$BACKUP_GCS_KEY_SECRET"
  [[ "${ROOT_COUNT[$name]:-0}" -eq 1 ]] || die "BACKUP_GCS_KEY_SECRET '$name' must appear exactly once in SECRETS_FILE (found ${ROOT_COUNT[$name]:-0})"
  scope="${ROOT_SCOPE[$name]}"
  [[ "$scope" == root || "$scope" == backup ]] || die "BACKUP_GCS_KEY_SECRET '$name' is scoped [$scope], so accounts would get the GCS service-account key. Scope it [root] (the provisioner installs it for the backup user) or [backup]"
  b64="${ROOT_VALUE[$name]}"
  BK_KEY_JSON="$(printf '%s' "$b64" | base64 -d 2>/dev/null)" || die "BACKUP_GCS_KEY_SECRET '$name' is not valid base64 (encode the key file on ONE line: base64 -w0 key.json)"
  jq -e '.type == "service_account" and (.private_key | type == "string") and (.client_email | type == "string")' <<<"$BK_KEY_JSON" >/dev/null 2>&1 \
    || die "BACKUP_GCS_KEY_SECRET '$name' does not decode to a service-account key JSON (type, private_key and client_email are required)"
  # None of these may reach an Ikenga account.
  for k in $used $name; do
    for a in "${ACCT_LOGINS[@]}"; do
      [[ -z "${SEC_SEEN[$a|$k]:-}" ]] || die "$k is a backup secret but SECRETS_FILE also delivers a secret of that name to account $a. Backup secrets must not reach any Ikenga account"
    done
  done
  # Written to the env file: the referenced connection strings only.
  for k in $(printf '%s\n' $used | sort); do BK_ENV_BODY+="$k=${BACKUP_VALUE[$k]}"$'\n'; done
  # Backup-scoped secrets nothing refers to: named, not written.
  local unused=()
  for k in $(printf '%s\n' "${!BACKUP_VALUE[@]}" | sort); do
    [[ "$used" == *" $k "* || "$k" == "$name" ]] || unused+=("$k")
  done
  [[ ${#unused[@]} -eq 0 ]] || note "backup-scoped secrets no database refers to (not written): ${unused[*]}"
  return 0
}

# ---- the user and its directories

backup_ensure_user() {
  local line uid lo hi shell home
  lo="${UID_RANGE%-*}"; hi="${UID_RANGE#*-}"
  if line="$(getent passwd "$BACKUP_USER")"; then
    IFS=: read -r _ _ uid _ _ home shell <<<"$line"
    (( uid > 0 )) || die "BACKUP_USER $BACKUP_USER has uid 0"
    (( uid < lo || uid >= hi )) || die "BACKUP_USER $BACKUP_USER has uid $uid, inside the Ikenga account range $UID_RANGE; it must be a plain system user"
    if [[ "$home" != "$BACKUP_PRIV_DIR" ]]; then
      run usermod -d "$BACKUP_PRIV_DIR" "$BACKUP_USER"; changed "user $BACKUP_USER: home -> $BACKUP_PRIV_DIR"
    fi
    if [[ "$shell" != /usr/sbin/nologin ]]; then
      run usermod -s /usr/sbin/nologin "$BACKUP_USER"; changed "user $BACKUP_USER: login shell -> nologin"
    fi
    # No supplementary groups: nothing the user could read through a group.
    if [[ "$(id -nG "$BACKUP_USER")" != "$(id -gn "$BACKUP_USER")" ]]; then
      run usermod -G "" "$BACKUP_USER"; changed "user $BACKUP_USER: supplementary groups removed"
    fi
  else
    run useradd --system --user-group --no-create-home --home-dir "$BACKUP_PRIV_DIR" --shell /usr/sbin/nologin \
      --comment "Ikenga database backups" "$BACKUP_USER"
    changed "system user $BACKUP_USER created (no login, home $BACKUP_PRIV_DIR)"
  fi
  if id "$BACKUP_USER" >/dev/null 2>&1; then
    BK_UID="$(id -u "$BACKUP_USER")"; BK_GID="$(id -g "$BACKUP_USER")"; BK_GROUP="$(id -gn "$BACKUP_USER")"
  else
    BK_UID=0; BK_GID=0; BK_GROUP="$BACKUP_USER"        # dry run, user not created yet
  fi
}

# Run a command as the backup user, with no supplementary groups and a clean
# environment: how the service runs. This is how root touches anything INSIDE
# status/ or private/: those directories belong to the backup user, so a path
# in them can be swapped for a symlink at any moment, and root must not follow it.
# HOME is a path the backup user does not control (and that does not exist),
# so tools root runs as that user never load user-supplied startup files (jq
# reads ~/.jq). Every call is time-limited, so a FIFO the user plants where one
# of these tools reads cannot hang provisioning.
as_backup() {
  ( cd / && exec timeout --kill-after=5 "${BACKUP_AS_USER_TIMEOUT:-120}" \
      setpriv --reuid="$BK_UID" --regid="$BK_GID" --clear-groups \
      env -i HOME=/nonexistent PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 "$@" )
}

# Sets BK_UID/BK_GID/BK_GROUP for a user that already exists (the disabled path
# never creates one). Returns 1 when there is no such user.
backup_load_ids() {
  [[ $EUID -eq 0 ]] || return 1          # an unprivileged dry run cannot look inside the user's directories
  id "$BACKUP_USER" >/dev/null 2>&1 || return 1
  BK_UID="$(id -u "$BACKUP_USER")"; BK_GID="$(id -g "$BACKUP_USER")"; BK_GROUP="$(id -gn "$BACKUP_USER")"
}

# ---- paths the backup user controls

# backup_safe_dir <mode> <owner> <group> <expected owner uid> <path>
# Creates the directory, or fixes the owner/mode of an existing one, but never
# through a symlink and never one somebody else owns: a symlink, a non-directory
# or a directory owned by an unexpected uid is REFUSED, not adopted. The parent
# must be root's (the caller guarantees it), so nothing can be swapped between
# the check and the chown/chmod. (stat without -L is an lstat.)
backup_safe_dir() {
  local mode="$1" owner="$2" group="$3" want_uid="$4" d="$5" kind cur
  kind="$(stat -c '%F' -- "$d" 2>/dev/null || true)"
  if [[ -z "$kind" ]]; then
    run install -d -m "$mode" -o "$owner" -g "$group" "$d"
    changed "$d created ($owner:$group $mode)"
    return 0
  fi
  [[ "$kind" != "symbolic link" ]] || die "$d is a symbolic link. Refusing to follow it: root would chown/chmod whatever it points at. Inspect it, remove it by hand, and run again."
  [[ "$kind" == directory ]] || die "$d exists and is a $kind, not a directory; refusing to touch it. Inspect it, move it away, and run again."
  cur="$(stat -c '%u %a %U %G' -- "$d")"
  [[ "${cur%% *}" == "$want_uid" ]] || die "$d is owned by uid ${cur%% *}, not $want_uid. Refusing to adopt it (an earlier layout, or tampering): inspect it, fix or remove it by hand, and run again."
  cur="${cur#* }"
  [[ "$cur" == "${mode#0} $owner $group" ]] && return 0
  run chown -h "$owner:$group" "$d"; run chmod "$mode" "$d"
  changed "$d: owner/mode set to $owner:$group $mode"
}

# The three directories, in order. STATE is root's alone; status/ and private/
# are the backup user's, and live directly under it.
backup_ensure_dirs() {
  backup_safe_dir 0755 root root 0 "$BACKUP_STATE_DIR"
  backup_safe_dir 0755 "$BACKUP_USER" "$BK_GROUP" "$BK_UID" "$BACKUP_STATUS_DIR"
  backup_safe_dir 0700 "$BACKUP_USER" "$BK_GROUP" "$BK_UID" "$BACKUP_PRIV_DIR"
  # status.json at the old, documented path is a root-made symlink into status/
  # (the user can't create files in STATE, so it can't atomically replace the
  # status file there; in status/ it can).
  local link="$BACKUP_STATE_DIR/status.json" kind
  kind="$(stat -c '%F' -- "$link" 2>/dev/null || true)"
  if [[ "$kind" == "symbolic link" && "$(readlink -- "$link")" == status/status.json ]]; then return 0; fi
  [[ "$kind" != directory ]] || die "$link is a directory; refusing to touch it. Move it away and run again."
  run ln -sfn status/status.json "$link"
  changed "$link -> status/status.json"
}

# Refuses a symlink at any path of the backup user's trees. Root's own lstat on
# the two directories, everything inside them looked at AS the backup user.
backup_refuse_symlinks() {
  local d p
  for d in "$BACKUP_STATE_DIR" "$BACKUP_STATUS_DIR" "$BACKUP_PRIV_DIR"; do
    [[ "$(stat -c '%F' -- "$d" 2>/dev/null || true)" != "symbolic link" ]] \
      || die "$d is a symbolic link. Refusing to follow it: root would chown/chmod whatever it points at. Inspect it, remove it by hand, and run again."
  done
  backup_load_ids || return 0
  for p in "$BACKUP_STATUS_DIR/status.json" "$BACKUP_STATUS_DIR/.status.lock" "$BACKUP_KEY_DST" \
           "$BACKUP_PRIV_DIR/errors" "$BACKUP_PRIV_DIR/work" "$BACKUP_GCLOUD_DIR"; do
    if as_backup test -L "$p"; then
      die "$p is a symbolic link (the backup user made it, or tampering). Refusing to follow it. Inspect it, remove it by hand, and run again."
    fi
  done
  if [[ -n "$(as_backup find "$BACKUP_GCLOUD_DIR/" -mindepth 1 -maxdepth 1 -type l -print -quit 2>/dev/null || true)" ]]; then
    die "$BACKUP_GCLOUD_DIR holds a symbolic link. Refusing to follow it. Inspect it, remove it by hand, and run again."
  fi
}

# ---- the units

backup_service_unit() {
  local gcloud_dir path
  gcloud_dir="$(dirname -- "$(command -v gcloud 2>/dev/null || echo /usr/bin/gcloud)")"
  path="/usr/local/bin:/usr/bin:/bin"
  case ":$path:" in *":$gcloud_dir:"*) ;; *) path="$gcloud_dir:$path" ;; esac
  cat <<EOF
[Unit]
Description=Ikenga: Postgres backup to GCS (%i)
After=network-online.target
Wants=network-online.target
ConditionPathExists=$BACKUP_CONFIG_DST

[Service]
Type=oneshot
User=$BACKUP_USER
Group=$BK_GROUP
ExecStart=$BACKUP_LIB_DIR/run-backup.sh --schedule %i
SyslogIdentifier=ikenga-backup
TimeoutStartSec=$BACKUP_TIMEOUT_SEC
Nice=10
IOSchedulingClass=idle
UMask=0077
Environment=HOME=$BACKUP_PRIV_DIR
Environment=PATH=$path
Environment=LANG=C.UTF-8
Environment=BACKUP_PG_MIN_MAJOR=$BACKUP_PG_MAJOR
Environment=CLOUDSDK_CONFIG=$BACKUP_GCLOUD_DIR/%i
# Sandbox: the job writes only its state directory; it cannot see the Ikenga
# daemon's data, the account secrets or the project mirrors.
NoNewPrivileges=true
PrivateTmp=true
PrivateDevices=true
ProtectSystem=strict
ReadWritePaths=$BACKUP_STATUS_DIR $BACKUP_PRIV_DIR
InaccessiblePaths=-/etc/ikenga -/opt/ikenga -/srv/ikenga -/root
ProtectHome=true
ProtectProc=invisible
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=true
ProtectClock=true
ProtectHostname=true
RestrictNamespaces=true
RestrictRealtime=true
RestrictSUIDSGID=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6 AF_NETLINK
LockPersonality=true
RemoveIPC=true
CapabilityBoundingSet=
AmbientCapabilities=
SystemCallArchitectures=native
SystemCallFilter=@system-service
EOF
}

backup_timer_unit() {   # schedule
  cat <<EOF
[Unit]
Description=Ikenga: Postgres backup timer ($1)

[Timer]
OnCalendar=${BK_CAL[$1]}
RandomizedDelaySec=120
AccuracySec=1min
Persistent=true
Unit=ikenga-backup@$1.service

[Install]
WantedBy=timers.target
EOF
}

# write_unit (above) returns 0 when it changed the file.
backup_sync_units() {
  local s u timer wanted=" " f
  if write_unit "$BACKUP_UNIT" "$(backup_service_unit)"; then BK_NEED_SYSTEMD_RELOAD=1; fi
  declare -A tchanged=()
  for s in "${BK_SCHEDS_USED[@]}"; do
    wanted+="$s "
    if write_unit "ikenga-backup-$s.timer" "$(backup_timer_unit "$s")"; then BK_NEED_SYSTEMD_RELOAD=1; tchanged[$s]=1; fi
  done
  # Timers of schedules no database uses any more.
  for f in "$SYSTEMD_DIR"/ikenga-backup-*.timer; do
    [[ -e "$f" ]] || continue
    u="$(basename -- "$f")"; s="${u#ikenga-backup-}"; s="${s%.timer}"
    [[ "$wanted" == *" $s "* ]] && continue
    if [[ $DRY_RUN -eq 0 ]]; then sc disable --now "$u" >/dev/null 2>&1 || true; rm -f -- "$f"; BK_NEED_SYSTEMD_RELOAD=1; fi
    changed "timer $u removed (no database uses schedule '$s')"
  done
  if [[ $DRY_RUN -eq 1 ]]; then
    note "[dry-run] systemctl daemon-reload; enable --now $(for s in "${BK_SCHEDS_USED[@]}"; do printf 'ikenga-backup-%s.timer ' "$s"; done)"
    return 0
  fi
  [[ $BK_NEED_SYSTEMD_RELOAD -eq 0 ]] || sc daemon-reload
  for s in "${BK_SCHEDS_USED[@]}"; do
    timer="ikenga-backup-$s.timer"
    if ! sc is-enabled --quiet "$timer" 2>/dev/null || ! sc is-active --quiet "$timer" 2>/dev/null; then
      sc enable --now "$timer" >/dev/null 2>&1 || soft_fail "could not enable $timer (see: systemctl status $timer)"
      changed "timer $timer enabled"
    elif [[ -n "${tchanged[$s]:-}" ]]; then
      # A changed OnCalendar only takes effect when the timer restarts.
      sc restart "$timer" >/dev/null 2>&1 || soft_fail "could not restart $timer"
      changed "timer $timer restarted with the new schedule"
    fi
  done
}

backup_remove_units() {
  local f u any=0
  for f in "$SYSTEMD_DIR"/ikenga-backup-*.timer; do
    [[ -e "$f" ]] || continue
    u="$(basename -- "$f")"
    if [[ $DRY_RUN -eq 0 ]]; then sc disable --now "$u" >/dev/null 2>&1 || true; rm -f -- "$f"; fi
    changed "timer $u removed"; any=1
  done
  if [[ -e "$SYSTEMD_DIR/$BACKUP_UNIT" ]]; then
    [[ $DRY_RUN -eq 1 ]] || rm -f -- "$SYSTEMD_DIR/$BACKUP_UNIT"
    changed "unit $BACKUP_UNIT removed"; any=1
  fi
  [[ $any -eq 0 || $DRY_RUN -eq 1 ]] || sc daemon-reload
}

# gcloud copies the service-account key into its config directory when it
# activates it (credentials.db, legacy_credentials/, access_tokens.db,
# configurations/). Removing the key file alone leaves that copy behind, so the
# whole per-schedule config tree goes: on disable, and when the key is rotated.
# Removed AS the backup user (its directory). The next run re-activates.
backup_remove_gcloud_creds() {   # reason
  backup_load_ids || return 0
  as_backup test -e "$BACKUP_GCLOUD_DIR" || return 0
  run as_backup rm -rf -- "$BACKUP_GCLOUD_DIR"
  changed "gcloud credentials removed ($BACKUP_GCLOUD_DIR: $1)"
}

# ---- the status file

# status.json lists, per database, the schedule it is on; it must match the
# config. (run-backup.sh --init-status does the writing, as the backup user.)
backup_status_matches() {
  local f="$BACKUP_STATUS_DIR/status.json" have want
  # Read AS the backup user: status/ is its directory, and root must not follow
  # whatever it has put there.
  backup_load_ids || return 1
  as_backup test -s "$f" || return 1
  have="$(as_backup jq -c '[.enabled, (.databases // {} | to_entries | map("\(.key):\(.value.schedule)") | sort)]' "$f" 2>/dev/null)" || return 1
  want="$(printf '%s\n' "${BK_NAMES[@]}" | while read -r n; do printf '%s:%s\n' "$n" "${BK_SCHED[$n]}"; done | LC_ALL=C sort | jq -R . | jq -sc '[true, .]')"
  [[ "$have" == "$want" ]]
}

backup_seed_status() {
  if backup_status_matches; then return 0; fi
  if [[ $DRY_RUN -eq 1 ]]; then changed "status file $BACKUP_STATUS_DIR/status.json initialised for ${#BK_NAMES[@]} database(s)"; return 0; fi
  as_backup BACKUP_CONFIG_FILE="$BACKUP_CONFIG_DST" BACKUP_STATE_DIR="$BACKUP_STATE_DIR" "$BACKUP_LIB_DIR/run-backup.sh" --init-status >/dev/null \
    || { soft_fail "could not initialise $BACKUP_STATUS_DIR/status.json"; return 0; }
  changed "status file $BACKUP_STATUS_DIR/status.json initialised for ${#BK_NAMES[@]} database(s)"
}

# ---- converge

sync_backups_enabled() {
  log "Database backups (user $BACKUP_USER; ${#BACKUP_SCHEDULES[@]} schedule override(s))"
  [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo)"
  local src="$SCRIPT_DIR/backup" f
  if [[ ! -f "$src/run-backup.sh" || ! -f "$src/verify-backup.sh" ]]; then
    [[ -x "$BACKUP_LIB_DIR/run-backup.sh" && -x "$BACKUP_LIB_DIR/verify-backup.sh" ]] \
      || die "backup scripts not found next to provision.sh ($src); run this from the repo checkout (scripts/server/)"
    note "backup scripts not next to this copy of provision.sh; keeping the installed ones in $BACKUP_LIB_DIR"
    src=""
  fi

  # Tools. jq first: everything below reads JSON.
  backup_apt_install jq gnupg curl ca-certificates gzip util-linux
  resolve_accounts
  load_secrets
  backup_load_config || return 0
  backup_plan_secrets || { note "secrets plan skipped (SECRETS_FILE not readable)"; }

  # pg_dump from PGDG: the servers are PostgreSQL 17, and a pg_dump older than
  # its server refuses to dump it.
  local codename; codename="$(. /etc/os-release && printf '%s' "${VERSION_CODENAME:-}")"
  [[ -n "$codename" ]] || die "cannot tell this release's codename from /etc/os-release (needed for the PGDG apt line)"
  backup_apt_repo pgdg "$PGDG_KEY_URL" "$PGDG_KEY_FPR" \
    "deb [signed-by={KEYRING}] https://apt.postgresql.org/pub/repos/apt ${codename}-pgdg main"
  backup_apt_install "postgresql-client-$BACKUP_PG_MAJOR"
  # gcloud (gcloud storage cp). An existing gcloud is used as it is.
  if command -v gcloud >/dev/null 2>&1; then
    note "gcloud found at $(command -v gcloud); not installing it"
  else
    backup_apt_repo gcloud "$GCLOUD_KEY_URL" "$GCLOUD_KEY_FPR ${BACKUP_GCLOUD_KEY_FPRS[*]^^}" \
      "deb [signed-by={KEYRING}] https://packages.cloud.google.com/apt cloud-sdk main"
    backup_apt_install google-cloud-cli
  fi

  # Refuse a symlink anywhere in the backup user's trees BEFORE anything is
  # changed (the user is the one who could have put it there).
  backup_refuse_symlinks
  backup_ensure_user
  ensure_dir 0755 root root "$BACKUP_LIB_DIR"
  ensure_dir 0750 root "$BK_GROUP" "$BACKUP_ETC_DIR"
  backup_ensure_dirs

  if [[ -n "$src" ]]; then
    for f in run-backup.sh verify-backup.sh; do install_file 0755 root root "$src/$f" "$BACKUP_LIB_DIR/$f" || true; done
  fi
  # The installed config is the profile's file plus "tunnel_ports" (see backup_render_config).
  local cfg_src; cfg_src="$(backup_render_config)"
  install_file 0640 root "$BK_GROUP" "$cfg_src" "$BACKUP_CONFIG_DST" || true
  [[ "$cfg_src" == "$BACKUP_CONFIG" ]] || rm -f -- "${cfg_src:?}"

  # The connection strings: root:<group> 0640, parsed by run-backup.sh.
  if [[ $SECRETS_UNREADABLE -eq 0 ]]; then
    local cur="" k diff=""
    [[ ! -f "$BACKUP_ENV_DST" ]] || cur="$(cat -- "$BACKUP_ENV_DST" 2>/dev/null || true)"
    parse_kv "$cur" OLD_KV; parse_kv "$BK_ENV_BODY" NEW_KV
    for k in $(printf '%s\n' "${!NEW_KV[@]}" | sort); do
      if [[ -z "${OLD_KV[$k]+x}" ]]; then diff+=" +$k"
      elif [[ "${OLD_KV[$k]}" != "${NEW_KV[$k]}" ]]; then diff+=" ~$k"; fi
    done
    for k in $(printf '%s\n' "${!OLD_KV[@]}" | sort); do [[ -n "${NEW_KV[$k]+x}" ]] || diff+=" -$k"; done
    if [[ -n "$diff" ]]; then
      if [[ $DRY_RUN -eq 0 ]]; then
        [[ ! -f "$BACKUP_ENV_DST" ]] || backup_secret_file "$BACKUP_ENV_DST"
        local tmp; tmp="$(mktemp "$BACKUP_ETC_DIR/.tmp.XXXXXX")"
        { printf '%s\n' "# ikenga: managed by provision.sh from SECRETS_FILE ([backup] scope); edits are overwritten"; printf '%s' "$BK_ENV_BODY"; } > "$tmp"
        chown "0:$BK_GID" "$tmp"; chmod 0640 "$tmp"; mv -f -- "$tmp" "$BACKUP_ENV_DST"
      fi
      changed "secrets $BACKUP_USER:$diff"
    else
      local o g m; read -r o g m < <(stat -c '%u %g %a' -- "$BACKUP_ENV_DST")
      if [[ "$o" != 0 || "$g" != "$BK_GID" || "$m" != 640 ]]; then
        run chown "0:$BK_GID" "$BACKUP_ENV_DST"; run chmod 0640 "$BACKUP_ENV_DST"; changed "secrets $BACKUP_USER: file owner/mode restored"
      fi
    fi
    # The GCS key: a 0600 file owned by the backup user, in its private directory.
    # private/ is the user's, so everything here is done AS the user: the file is
    # compared, its mode checked and the new one written by the user, from stdin
    # (the key is never in argv).
    local have_key=0 same_key=0 km
    if [[ $DRY_RUN -eq 0 || -d "$BACKUP_PRIV_DIR" ]] && backup_load_ids && as_backup test -f "$BACKUP_KEY_DST"; then
      have_key=1
      [[ "$(as_backup cat -- "$BACKUP_KEY_DST" 2>/dev/null || true)" == "$BK_KEY_JSON" ]] && same_key=1
    fi
    if [[ $same_key -eq 1 ]]; then
      km="$(as_backup stat -c '%a' -- "$BACKUP_KEY_DST")"
      if [[ "$km" != 600 ]]; then
        run as_backup chmod 0600 "$BACKUP_KEY_DST"; changed "GCS key file mode restored"
      fi
    else
      if [[ $DRY_RUN -eq 0 ]]; then
        printf '%s\n' "$BK_KEY_JSON" | as_backup BACKUP_STATE_DIR="$BACKUP_STATE_DIR" "$BACKUP_LIB_DIR/run-backup.sh" --install-key \
          || die "could not install the GCS key file $BACKUP_KEY_DST"
      fi
      changed "GCS key file ($BACKUP_GCS_KEY_SECRET -> $BACKUP_KEY_DST)"
      if [[ $have_key -eq 1 ]]; then
        # A rotated key: the gcloud credentials the old one left behind (the
        # service-account key is copied into CLOUDSDK_CONFIG) go with it.
        backup_remove_gcloud_creds "old key replaced"
      fi
    fi
    if [[ ${#BK_MISSING[@]} -gt 0 ]]; then
      soft_fail "no [backup] line in SECRETS_FILE for: ${BK_MISSING[*]} (database:secret). Those databases will report no-secret until it is added"
    fi
  fi

  backup_sync_units
  backup_seed_status
}

sync_backups_disabled() {
  backups_installed || [[ -d "$BACKUP_ETC_DIR" || -d "$BACKUP_STATE_DIR" ]] || return 0
  log "Database backups (disabled: timers and credentials removed, state and logs kept)"
  [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo)"
  # Root-owned things go FIRST, before any check the backup user can influence:
  # a symlink planted in its own tree must not be able to keep the timers
  # running or the connection strings on disk by making disable refuse.
  backup_remove_units
  # connections.env is root's. The key, the gcloud credentials and the status
  # file are in the backup user's directories, so they are removed / edited AS the
  # user, never by root through a path the user could have swapped.
  if [[ -f "$BACKUP_ENV_DST" ]]; then
    if [[ $DRY_RUN -eq 0 ]]; then backup_secret_file "$BACKUP_ENV_DST"; rm -f -- "$BACKUP_ENV_DST"; fi
    changed "$(basename -- "$BACKUP_ENV_DST") removed (credentials are not kept while backups are disabled)"
  fi
  backup_refuse_symlinks
  if backup_load_ids; then
    if as_backup test -e "$BACKUP_KEY_DST"; then
      run as_backup rm -f -- "$BACKUP_KEY_DST"
      changed "$(basename -- "$BACKUP_KEY_DST") removed (credentials are not kept while backups are disabled)"
    fi
    backup_remove_gcloud_creds "backups disabled"
    local stfile="$BACKUP_STATUS_DIR/status.json"
    if as_backup test -s "$stfile" && as_backup jq -e '.enabled == true' "$stfile" >/dev/null 2>&1; then
      run as_backup BACKUP_STATE_DIR="$BACKUP_STATE_DIR" "$BACKUP_LIB_DIR/run-backup.sh" --mark-disabled \
        || { soft_fail "could not mark $stfile disabled"; return 0; }
      changed "status file marked enabled=false"
    fi
  fi
}

sync_backups() {
  if [[ "$BACKUPS_ENABLED" == 1 ]]; then sync_backups_enabled; else sync_backups_disabled; fi
}

# ------------------------------------------------------------- SSH tunnels
#
# A tunnel is a systemd unit that keeps `ssh -N -L 127.0.0.1:<port>:<host>:<port>`
# up to a remote host, as an unprivileged system user, with a PINNED host key.
# One user and ONE key per box serve every tunnel. Layout (the unit names it):
#
#   /var/lib/<TUNNEL_USER>/                  <user> 0700   (the user's home; /var/lib is root's)
#     .ssh/                                  <user> 0700
#       id_ed25519, id_ed25519.pub           <user> 0600 / 0644. Generated ONCE, never regenerated.
#       known_hosts                          <user> 0644   the profile's TUNNEL_KNOWN_HOSTS, nothing else
#   /etc/systemd/system/<name>-tunnel.service  root       one per TUNNELS entry
#
# Root never follows or writes through a path the tunnel user controls: the
# home hangs off a root-owned parent (the user cannot swap it for a symlink),
# everything root does INSIDE it (look at, generate, compare, write) is done AS
# the tunnel user (as_tunnel), and a symlink at any of those paths is refused.
# Units are root's, in a root-owned directory.
#
# The remote end is not ours to configure: the key must be authorised there
# with the line this prints (restrict,port-forwarding,permitopen=...,from=...).

TUNNEL_HOME="/var/lib/$TUNNEL_USER"
TUNNEL_SSH_DIR="$TUNNEL_HOME/.ssh"
TUNNEL_KEY="$TUNNEL_SSH_DIR/id_ed25519"
TUNNEL_PUB="$TUNNEL_KEY.pub"
TUNNEL_KH="$TUNNEL_SSH_DIR/known_hosts"
TN_NAMES=(); declare -A TN_RUSER=() TN_RHOST=() TN_RPORT=() TN_LPORT=() TN_THOST=() TN_TPORT=() TN_KHKEY=()
TN_KH_ALL=(); TN_KH_USED=()
TN_UID=""; TN_GID=""; TN_HAVE_USER=0
TN_PUBLINE=""            # "ssh-ed25519 AAAA... comment": the public half of the key, once it exists
TN_KH_CHANGED=0
TN_ALLOW=()              # the names that may connect to a tunnel's local port (root always may); see "Port lock"

# The entry as it may be shown: quoted, truncated, no control characters.
tn_show() { local q; q="$(printf '%q' "${1:0:80}")"; printf '%s' "$q"; }

# Cheap checks that need nothing installed. Every field is matched against a
# strict pattern, so nothing from the profile can reach a unit file or ssh as
# anything but the one thing it names (no option, no shell, no extra line).
validate_tunnels_profile() {
  [[ "$TUNNEL_USER" =~ ^[a-z_][a-z0-9_-]{0,30}$ ]] || die "TUNNEL_USER '$(tn_show "$TUNNEL_USER")' is not a valid unix user name"
  case "$TUNNEL_USER" in
    root|ikenga|nobody|postgres|"$ADMIN_USER"|"$BACKUP_USER"|ik-*) die "TUNNEL_USER '$TUNNEL_USER' is not allowed: it must be its own plain system user (not root, the admin, the T0 'ikenga' user, the backup user, or an ik-* Ikenga account)" ;;
  esac
  [[ "$UID_RANGE" =~ ^[0-9]+-[0-9]+$ ]] || die "UID_RANGE must look like 20000-29999 (got '$UID_RANGE')"
  [[ -z "$TUNNEL_FROM" || "$TUNNEL_FROM" =~ ^[0-9A-Fa-f.:]{2,45}(/[0-9]{1,3})?$ ]] || die "TUNNEL_FROM must be one IP address or CIDR (got '$(tn_show "$TUNNEL_FROM")')"

  TN_NAMES=(); TN_RUSER=(); TN_RHOST=(); TN_RPORT=(); TN_LPORT=(); TN_THOST=(); TN_TPORT=(); TN_KHKEY=(); TN_KH_ALL=(); TN_KH_USED=()
  local n=0 e name ruser rhost rport lport thost tport hostre portre
  hostre='[A-Za-z0-9][A-Za-z0-9.-]{0,251}'; portre='[1-9][0-9]{0,4}'
  local entre="^([a-z0-9][a-z0-9-]{0,31})=([a-z_][a-z0-9_-]{0,31})@(${hostre})(:(${portre}))? (${portre}):(${hostre}):(${portre})\$"
  local seen_names=" " seen_ports=" "
  for e in "${TUNNELS[@]}"; do
    n=$((n + 1))
    [[ "$e" =~ $entre ]] || die "TUNNELS entry $n ($(tn_show "$e")) must look like: name=user@host[:sshport] localport:remotehost:remoteport   (name: a-z 0-9 -; host: letters, digits . -; ports 1-65535; exactly one space)"
    name="${BASH_REMATCH[1]}"; ruser="${BASH_REMATCH[2]}"; rhost="${BASH_REMATCH[3]}"; rport="${BASH_REMATCH[5]:-22}"
    lport="${BASH_REMATCH[6]}"; thost="${BASH_REMATCH[7]}"; tport="${BASH_REMATCH[8]}"
    local p
    for p in "$rport" "$lport" "$tport"; do
      (( 10#$p >= 1 && 10#$p <= 65535 )) || die "TUNNELS entry $n ('$name'): port $p is outside 1-65535"
    done
    [[ "$seen_names" != *" $name "* ]] || die "TUNNELS names '$name' twice (the unit would be $name-tunnel.service)"
    [[ "$seen_ports" != *" $lport "* ]] || die "TUNNELS entry $n ('$name'): local port $lport is already used by another tunnel"
    seen_names+="$name "; seen_ports+="$lport "
    (( 10#$lport >= 1024 )) || note "TUNNELS '$name': local port $lport is below 1024; the unprivileged tunnel user cannot bind it unless net.ipv4.ip_unprivileged_port_start is lowered" >&2
    TN_NAMES+=("$name"); TN_RUSER[$name]="$ruser"; TN_RHOST[$name]="$rhost"; TN_RPORT[$name]="$rport"
    TN_LPORT[$name]="$lport"; TN_THOST[$name]="$thost"; TN_TPORT[$name]="$tport"
    if [[ "$rport" == 22 ]]; then TN_KHKEY[$name]="$rhost"; else TN_KHKEY[$name]="[$rhost]:$rport"; fi
  done

  # Pinned host keys. No TOFU and no accept-new: an entry here, or no tunnel.
  local khre="^(\\[${hostre}\\]:${portre}|${hostre}) (ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp(256|384|521)) ([A-Za-z0-9+/]{16,2000}={0,2})\$"
  local line host type b64 k
  n=0
  for line in "${TUNNEL_KNOWN_HOSTS[@]}"; do
    n=$((n + 1))
    [[ "$line" =~ $khre ]] || die "TUNNEL_KNOWN_HOSTS entry $n ($(tn_show "$line")) must be exactly: <host|[host]:port> <ssh-ed25519|ecdsa-sha2-nistp256|ecdsa-sha2-nistp384|ecdsa-sha2-nistp521|ssh-rsa> <base64 key>"
    host="${BASH_REMATCH[1]}"; type="${BASH_REMATCH[2]}"; b64="${BASH_REMATCH[4]}"
    TN_KH_ALL+=("$host $type $b64")
  done
  for name in "${TN_NAMES[@]}"; do
    k="${TN_KHKEY[$name]}"; host=""
    for line in "${TN_KH_ALL[@]}"; do [[ "${line%% *}" == "$k" ]] && host=1; done
    [[ -n "$host" ]] || die "tunnel '$name' goes to $k but TUNNEL_KNOWN_HOSTS has no key for it. Host keys are pinned, never learned: add the line from the remote host (ssh-keyscan -t ed25519 ${TN_RHOST[$name]}), and confirm its fingerprint out of band"
  done
  # What is written to known_hosts: the entries some tunnel uses, in profile order.
  for line in "${TN_KH_ALL[@]}"; do
    for name in "${TN_NAMES[@]}"; do
      if [[ "${line%% *}" == "${TN_KHKEY[$name]}" ]]; then
        [[ " ${TN_KH_USED[*]:-} " == *" $line "* ]] || TN_KH_USED+=("$line")
        break
      fi
    done
  done

  # Who may connect to a tunnel's local port (root always may). Unset: the
  # backup user when backups are enabled, else nobody but root.
  TN_ALLOW=()
  local -a want_allow=(); local u seen_u=" "
  if [[ $TUNNEL_ALLOW_DEFINED -eq 1 ]]; then
    want_allow=("${TUNNEL_ALLOW_USERS[@]}")
  elif [[ "$BACKUPS_ENABLED" == 1 ]]; then
    want_allow=("$BACKUP_USER")
  fi
  n=0
  for u in "${want_allow[@]}"; do
    n=$((n + 1))
    [[ "$u" =~ ^[a-z_][a-z0-9_-]{0,30}$ ]] || die "TUNNEL_ALLOW_USERS entry $n ($(tn_show "$u")) is not a valid unix user name"
    [[ "$u" != root ]] || continue          # root is always allowed
    [[ "$seen_u" != *" $u "* ]] || continue
    seen_u+="$u "; TN_ALLOW+=("$u")
  done
  return 0
}

# The deep check of the host keys needs ssh-keygen, so it runs after the packages.
tunnel_check_host_keys() {
  command -v ssh-keygen >/dev/null 2>&1 || { note "ssh-keygen is not installed yet; the pinned host keys are parsed once openssh-client is"; return 0; }
  local d line type out want
  d="$(mktemp -d)"
  for line in "${TN_KH_USED[@]}"; do
    printf '%s\n' "$line" > "$d/kh"
    out="$(ssh-keygen -l -f "$d/kh" 2>/dev/null)" || { rm -rf -- "$d"; die "TUNNEL_KNOWN_HOSTS: the key given for ${line%% *} is not a valid SSH public key"; }
    type="$(cut -d' ' -f2 <<<"$line")"
    case "$type" in ssh-ed25519) want="(ED25519)" ;; ssh-rsa) want="(RSA)" ;; *) want="(ECDSA)" ;; esac
    [[ "$out" == *" $want" ]] || { rm -rf -- "$d"; die "TUNNEL_KNOWN_HOSTS: the key for ${line%% *} is not a $type key"; }
  done
  rm -rf -- "$d"
}

tunnel_load_ids() {
  TN_HAVE_USER=0
  [[ $EUID -eq 0 ]] || return 1          # an unprivileged dry run cannot look inside the user's directories
  id "$TUNNEL_USER" >/dev/null 2>&1 || return 1
  TN_UID="$(id -u "$TUNNEL_USER")"; TN_GID="$(id -g "$TUNNEL_USER")"; TN_HAVE_USER=1
}

# Run a command as the tunnel user: no supplementary groups, clean environment,
# HOME a path the user does not control (ssh-keygen and friends never read a
# user-supplied startup file), time-limited so a FIFO planted where a tool
# reads cannot hang provisioning. How root touches anything INSIDE the home.
as_tunnel() {
  ( cd / && exec timeout --kill-after=5 "${TUNNEL_AS_USER_TIMEOUT:-60}" \
      setpriv --reuid="$TN_UID" --regid="$TN_GID" --clear-groups \
      env -i HOME=/nonexistent PATH=/usr/local/bin:/usr/bin:/bin LANG=C.UTF-8 "$@" )
}

tunnel_refuse_symlinks() {
  local p
  [[ "$(stat -c '%F' -- "$TUNNEL_HOME" 2>/dev/null || true)" != "symbolic link" ]] \
    || die "$TUNNEL_HOME is a symbolic link. Refusing to follow it: root would chown/chmod whatever it points at. Inspect it, remove it by hand, and run again."
  tunnel_load_ids || return 0
  for p in "$TUNNEL_SSH_DIR" "$TUNNEL_KEY" "$TUNNEL_PUB" "$TUNNEL_KH"; do
    if as_tunnel test -L "$p"; then
      die "$p is a symbolic link (the tunnel user made it, or tampering). Refusing to follow it. Inspect it, remove it by hand, and run again."
    fi
  done
}

tunnel_ensure_user() {
  local line uid lo hi shell home
  lo="${UID_RANGE%-*}"; hi="${UID_RANGE#*-}"
  if line="$(getent passwd "$TUNNEL_USER")"; then
    IFS=: read -r _ _ uid _ _ home shell <<<"$line"
    (( uid > 0 )) || die "TUNNEL_USER $TUNNEL_USER has uid 0"
    (( uid < lo || uid >= hi )) || die "TUNNEL_USER $TUNNEL_USER has uid $uid, inside the Ikenga account range $UID_RANGE; it must be a plain system user"
    if [[ "$home" != "$TUNNEL_HOME" ]]; then
      run usermod -d "$TUNNEL_HOME" "$TUNNEL_USER"; changed "user $TUNNEL_USER: home -> $TUNNEL_HOME"
    fi
    if [[ "$shell" != /usr/sbin/nologin ]]; then
      run usermod -s /usr/sbin/nologin "$TUNNEL_USER"; changed "user $TUNNEL_USER: login shell -> nologin"
    fi
    if [[ "$(id -nG "$TUNNEL_USER")" != "$(id -gn "$TUNNEL_USER")" ]]; then
      run usermod -G "" "$TUNNEL_USER"; changed "user $TUNNEL_USER: supplementary groups removed"
    fi
  else
    run useradd --system --user-group --no-create-home --home-dir "$TUNNEL_HOME" --shell /usr/sbin/nologin \
      --comment "Ikenga SSH tunnels" "$TUNNEL_USER"
    changed "system user $TUNNEL_USER created (no login, home $TUNNEL_HOME)"
  fi
  tunnel_load_ids || true
}

tunnel_ensure_ssh_dir() {
  local d="$TUNNEL_SSH_DIR" info owner mode
  if [[ $TN_HAVE_USER -eq 1 ]] && info="$(as_tunnel stat -c '%F|%u|%a' -- "$d" 2>/dev/null)"; then
    [[ "${info%%|*}" == directory ]] || die "$d exists and is a ${info%%|*}, not a directory; refusing to touch it. Move it away and run again."
    owner="$(cut -d'|' -f2 <<<"$info")"; mode="${info##*|}"
    [[ "$owner" == "$TN_UID" ]] || die "$d is owned by uid $owner, not $TN_UID ($TUNNEL_USER). Refusing to adopt it: inspect it, fix or remove it by hand, and run again."
    if [[ "$mode" != 700 ]]; then run as_tunnel chmod 0700 -- "$d"; changed "$d: mode set to 0700"; fi
  else
    run as_tunnel install -d -m 0700 -- "$d"
    changed "$d created ($TUNNEL_USER 0700)"
  fi
}

# One key per box. Generated once and NEVER regenerated: an existing one is
# adopted when it is a regular ed25519 key of this user (a symlink or another
# owner is refused). Sets TN_PUBLINE.
tunnel_ensure_key() {
  local info owner mode pub have_pub fp h
  TN_PUBLINE=""
  if [[ $TN_HAVE_USER -eq 1 ]] && as_tunnel test -e "$TUNNEL_KEY"; then
    info="$(as_tunnel stat -c '%F|%u|%a' -- "$TUNNEL_KEY")"
    [[ "${info%%|*}" == "regular file" ]] || die "$TUNNEL_KEY is a ${info%%|*}, not a key file; refusing to touch it. Inspect it, move it away, and run again."
    owner="$(cut -d'|' -f2 <<<"$info")"; mode="${info##*|}"
    [[ "$owner" == "$TN_UID" ]] || die "$TUNNEL_KEY is owned by uid $owner, not $TN_UID ($TUNNEL_USER). Refusing to adopt it (tampering, or another layout): inspect it, fix or remove it by hand, and run again."
    if (( (8#$mode & 8#077) != 0 )); then
      run as_tunnel chmod 0600 -- "$TUNNEL_KEY"; changed "$TUNNEL_KEY: mode set to 0600"
    fi
    # The public half, derived by the key's owner. Nothing private is printed.
    pub="$(as_tunnel ssh-keygen -y -P '' -f "$TUNNEL_KEY" 2>/dev/null </dev/null)" \
      || die "$TUNNEL_KEY cannot be read as an SSH private key without a passphrase; refusing to touch it"
    [[ "$pub" == "ssh-ed25519 "* ]] || die "$TUNNEL_KEY is not an ed25519 key; refusing to touch it"
    TN_PUBLINE="$pub"
    have_pub=""
    if as_tunnel test -f "$TUNNEL_PUB"; then
      have_pub="$(as_tunnel cat -- "$TUNNEL_PUB" 2>/dev/null | cut -d' ' -f1,2 || true)"
    fi
    if [[ "$have_pub" != "$(cut -d' ' -f1,2 <<<"$pub")" ]]; then
      if as_tunnel test -e "$TUNNEL_PUB"; then
        info="$(as_tunnel stat -c '%F|%u' -- "$TUNNEL_PUB")"
        [[ "$info" == "regular file|$TN_UID" ]] || die "$TUNNEL_PUB is not a regular file of $TUNNEL_USER; refusing to touch it"
      fi
      if [[ $DRY_RUN -eq 0 ]]; then
        printf '%s\n' "$pub" | as_tunnel sh -c 'umask 022; cat > "$1.new" && mv -f "$1.new" "$1"' sh "$TUNNEL_PUB"
      fi
      changed "$TUNNEL_PUB written (derived from the existing key)"
    fi
    return 0
  fi
  h="$(hostname -s 2>/dev/null || echo box)"; [[ "$h" =~ ^[A-Za-z0-9._-]+$ ]] || h=box
  run as_tunnel ssh-keygen -q -t ed25519 -N '' -C "$TUNNEL_USER@$h" -f "$TUNNEL_KEY"
  if [[ $DRY_RUN -eq 0 ]]; then
    TN_PUBLINE="$(as_tunnel ssh-keygen -y -P '' -f "$TUNNEL_KEY" </dev/null)" || die "could not read back the key just generated"
    fp="$(as_tunnel ssh-keygen -l -f "$TUNNEL_PUB" | cut -d' ' -f2)"
    changed "$TUNNEL_KEY generated (ed25519, $fp); it is never regenerated"
  else
    changed "$TUNNEL_KEY generated (ed25519)"
  fi
}

# known_hosts is exactly the profile's entries for the hosts some tunnel goes
# to. Compared as a set of lines (order, blanks and comments do not matter) and
# written by the tunnel user. A replaced file is first copied aside.
tunnel_kh_norm() { { grep -v '^[[:space:]]*#' || true; } | tr -s ' \t' ' ' | sed -E '/^ *$/d; s/^ //; s/ $//' | LC_ALL=C sort -u; }
tunnel_sync_known_hosts() {
  local want have info owner mode
  TN_KH_CHANGED=0
  want="$(printf '%s\n' "${TN_KH_USED[@]}")"
  if [[ $TN_HAVE_USER -eq 1 ]] && as_tunnel test -e "$TUNNEL_KH"; then
    info="$(as_tunnel stat -c '%F|%u|%a' -- "$TUNNEL_KH")"
    [[ "${info%%|*}" == "regular file" ]] || die "$TUNNEL_KH is a ${info%%|*}, not a file; refusing to touch it. Inspect it, move it away, and run again."
    owner="$(cut -d'|' -f2 <<<"$info")"; mode="${info##*|}"
    [[ "$owner" == "$TN_UID" ]] || die "$TUNNEL_KH is owned by uid $owner, not $TN_UID ($TUNNEL_USER). Refusing to adopt it: inspect it, fix or remove it by hand, and run again."
    have="$(as_tunnel cat -- "$TUNNEL_KH" 2>/dev/null || true)"
    if [[ "$(tunnel_kh_norm <<<"$have")" == "$(tunnel_kh_norm <<<"$want")" ]]; then
      if (( (8#$mode & 8#022) != 0 )); then
        run as_tunnel chmod 0644 -- "$TUNNEL_KH"; changed "$TUNNEL_KH: mode set to 0644"
      fi
      return 0
    fi
    if [[ $DRY_RUN -eq 0 ]]; then
      as_tunnel cp -p -- "$TUNNEL_KH" "$TUNNEL_KH.bak-$(date +%Y%m%d-%H%M%S)" || die "could not back up $TUNNEL_KH"
    fi
  fi
  if [[ $DRY_RUN -eq 0 ]]; then
    printf '%s\n' "$want" | as_tunnel sh -c 'umask 022; cat > "$1.new" && chmod 0644 "$1.new" && mv -f "$1.new" "$1"' sh "$TUNNEL_KH" \
      || die "could not write $TUNNEL_KH"
  fi
  TN_KH_CHANGED=1
  changed "$TUNNEL_KH written (${#TN_KH_USED[@]} pinned host key(s), from TUNNEL_KNOWN_HOSTS)"
}

# ---- the units

tunnel_unit_name() { printf '%s-tunnel.service' "$1"; }

tunnel_unit() {   # name
  local n="$1"
  printf '%s\n' "# Managed by ikenga provision.sh (tunnels): change TUNNELS in the profile, not this file." \
    "[Unit]" \
    "Description=Ikenga: SSH tunnel $n (127.0.0.1:${TN_LPORT[$n]} to ${TN_THOST[$n]}:${TN_TPORT[$n]} via ${TN_RUSER[$n]}@${TN_RHOST[$n]})" \
    "Documentation=https://github.com/ikenga-hq/ikenga/blob/main/scripts/server/README.md" \
    "After=network-online.target" \
    "Wants=network-online.target" \
    "" \
    "[Service]" \
    "User=$TUNNEL_USER" \
    "Group=$TUNNEL_USER"
  printf 'ExecStart=/usr/bin/ssh -NT \\\n'
  # -F none: never read ~/.ssh/config, so nothing in the tunnel user's home can
  # add a ProxyCommand, a GlobalKnownHostsFile or an extra forward.
  printf '  -F none \\\n'
  printf '  -i %s \\\n' "$TUNNEL_KEY"
  printf '  -o UserKnownHostsFile=%s \\\n' "$TUNNEL_KH"
  printf '  -o %s \\\n' StrictHostKeyChecking=yes IdentitiesOnly=yes ExitOnForwardFailure=yes ServerAliveInterval=30 ServerAliveCountMax=3 BatchMode=yes
  if [[ "${TN_RPORT[$n]}" != 22 ]]; then printf '  -p %s \\\n' "${TN_RPORT[$n]}"; fi
  printf '  -L 127.0.0.1:%s:%s:%s \\\n' "${TN_LPORT[$n]}" "${TN_THOST[$n]}" "${TN_TPORT[$n]}"
  printf '  %s@%s\n' "${TN_RUSER[$n]}" "${TN_RHOST[$n]}"
  printf '%s\n' "Restart=always" "RestartSec=10" "NoNewPrivileges=yes" "ProtectSystem=strict" "ProtectHome=yes" "PrivateTmp=yes" \
    "ReadOnlyPaths=$TUNNEL_HOME" \
    "" \
    "[Install]" \
    "WantedBy=multi-user.target"
}

# A unit as systemd sees it, minus what does not change behaviour: comments,
# blank lines, continuation formatting, Description= and Documentation=. This is
# what lets a hand-made unit with the same effect be ADOPTED without a rewrite
# or a restart.
tunnel_unit_norm() {
  awk '
    /^[ \t]*[#;]/ { next }
    { line = $0; sub(/[ \t]+$/, "", line)
      if (line ~ /\\$/) { sub(/\\$/, "", line); buf = buf line " "; next }
      print buf line; buf = "" }
  ' | sed -E '/^[[:space:]]*$/d; /^(Description|Documentation)=/d; s/[[:space:]]+/ /g; s/^ //; s/ $//'
}

# Writes the unit when its effective content differs. Returns 0 when it changed it.
TN_UCHANGED=" "
tunnel_write_unit() {   # name
  local n="$1" u f want have ts
  u="$(tunnel_unit_name "$n")"; f="$SYSTEMD_DIR/$u"
  want="$(tunnel_unit "$n")"
  [[ ! -L "$f" ]] || die "$f is a symbolic link. Refusing to write through it. Inspect it, remove it by hand, and run again."
  if [[ -e "$f" ]]; then
    [[ -f "$f" ]] || die "$f exists and is not a regular file; refusing to touch it"
    if [[ "$(tunnel_unit_norm < "$f")" == "$(tunnel_unit_norm <<<"$want")" ]]; then
      if [[ "$(stat -c '%a %u %g' -- "$f")" != "644 0 0" ]]; then
        run chown 0:0 "$f"; run chmod 0644 "$f"; changed "unit $u: owner/mode set to root 0644"
      fi
      return 1
    fi
    ts="$(date +%Y%m%d-%H%M%S)"
    if [[ $DRY_RUN -eq 0 ]]; then cp -p -- "$f" "$f.bak-$ts"; fi
    note "$u differs from the profile's tunnel; the old unit is kept as $u.bak-$ts"
  fi
  if [[ $DRY_RUN -eq 1 ]]; then
    printf '    [dry-run] write %s:\n' "$f"
    printf '%s\n' "$want" | sed 's/^/      | /'
  else
    local tmp; tmp="$(mktemp "$SYSTEMD_DIR/.tmp.XXXXXX")"
    printf '%s\n' "$want" > "$tmp"; chown 0:0 "$tmp"; chmod 0644 "$tmp"; mv -f -- "$tmp" "$f"
  fi
  TN_UCHANGED+="$n "
  changed "unit $u installed"
  return 0
}

# Units of TUNNEL_USER that the profile no longer lists: stopped, disabled,
# removed. The key, the user and known_hosts stay.
tunnel_stale_units() {
  local f u n
  for f in "$SYSTEMD_DIR"/*-tunnel.service; do
    [[ -e "$f" || -L "$f" ]] || continue
    u="$(basename -- "$f")"; n="${u%-tunnel.service}"
    [[ " ${TN_NAMES[*]:-} " == *" $n "* ]] && continue
    if [[ -L "$f" ]]; then note "$u is a symbolic link; leaving it alone" >&2; continue; fi
    grep -qFx "User=$TUNNEL_USER" "$f" 2>/dev/null || continue
    printf '%s\n' "$u"
  done
}

tunnel_remove_stale() {
  local u any=0
  while IFS= read -r u; do
    [[ -n "$u" ]] || continue
    if [[ $DRY_RUN -eq 0 ]]; then
      sc disable --now "$u" >/dev/null 2>&1 || true
      rm -f -- "${SYSTEMD_DIR:?}/${u:?}" "${SYSTEMD_DIR:?}/${u:?}.d/${TUNNEL_LOCK_DROPIN:?}"
      rmdir -- "${SYSTEMD_DIR:?}/${u:?}.d" 2>/dev/null || true
    fi
    changed "unit $u stopped, disabled and removed (not in TUNNELS; the key is kept)"; any=1
  done < <(tunnel_stale_units)
  [[ $any -eq 0 || $DRY_RUN -eq 1 ]] || sc daemon-reload
}

# The address the remote host will see this box connect from.
tunnel_from_ip() {
  local ip
  if [[ -n "$TUNNEL_FROM" ]]; then printf '%s' "$TUNNEL_FROM"; return; fi
  ip="$(ip -4 route get 1.1.1.1 2>/dev/null | grep -oE 'src [0-9.]+' | awk '{print $2}' | head -n1 || true)"
  if [[ -z "$ip" || "$ip" =~ ^(10\.|127\.|169\.254\.|192\.168\.|172\.(1[6-9]|2[0-9]|3[01])\.|100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.) ]]; then
    printf '%s' "<THIS-BOX-PUBLIC-IP>"
  else
    printf '%s' "$ip"
  fi
}

tunnel_print_remote_lines() {
  local n from line
  from="$(tunnel_from_ip)"
  note "Install on each remote host (the provisioner cannot): one line in the tunnel user's authorized_keys there."
  [[ "$from" != "<THIS-BOX-PUBLIC-IP>" ]] || note "This box's public address could not be determined: replace <THIS-BOX-PUBLIC-IP> (or set TUNNEL_FROM in the profile)."
  for n in "${TN_NAMES[@]}"; do
    note "$n: ${TN_RUSER[$n]}@${TN_RHOST[$n]}$([[ ${TN_RPORT[$n]} == 22 ]] || printf ' (ssh port %s)' "${TN_RPORT[$n]}")"
    line="restrict,port-forwarding,permitopen=\"${TN_THOST[$n]}:${TN_TPORT[$n]}\",from=\"$from\" ${TN_PUBLINE:-<public key: shown once the key exists>}"
    printf '      %s\n' "$line"
  done
  if [[ -n "$TN_PUBLINE" && $DRY_RUN -eq 0 ]]; then
    note "key fingerprint: $(printf '%s\n' "$TN_PUBLINE" | ssh-keygen -l -f - 2>/dev/null | cut -d' ' -f2 || true)"
  fi
}

# ---- the port lock
#
# A tunnel's local end is 127.0.0.1:<port>, and loopback is open to every local
# account, people and agents alike: any of them could connect to it and talk to
# the remote database, and while the tunnel is down any of them could bind the
# port and receive the backup job's connection, login included. So the connect
# side is locked to uids: an nftables table of its own, `inet ikenga_tunnels`,
# with, per tunnel port and per family (127.0.0.1 and ::1), an ALLOW rule for the
# uids of root and TN_ALLOW (TUNNEL_ALLOW_USERS, default the backup user) followed
# by a REJECT rule for everybody else. Fail closed on purpose: the obvious
# `meta skuid != { allowed } reject` does not match a socket that has no owning
# file (AF_SMC connects through a kernel-internal TCP socket of that kind), so
# such a socket would walk straight through; "accept the allowed uids, reject
# whatever is left" cannot be bypassed that way.
#
# Why a table of its own and not ufw: ufw cannot match on the local user, and
# `ufw --force reset` (the perimeter step does it) rebuilds ufw's own chains only,
# so a separate nft table is never touched by it. Why not nftables.service and
# /etc/nftables.conf: the stock file starts with `flush ruleset`, which would wipe
# ufw's tables whenever it was restarted. So the table is loaded by its own
# oneshot unit ($TUNNEL_LOCK_UNIT, WantedBy sysinit.target, Before
# network-pre.target) from $TUNNEL_LOCK_FILE. That file is `table X` /
# `delete table X` / `table X { ... }`: one nft transaction, so a reload swaps the
# rules atomically and never opens a gap.
#
# Fail closed: every tunnel unit gets a drop-in that Requires and orders After the
# lock unit, so a tunnel does not come up (and stops) without its lock. There is
# deliberately no ConditionPathExists on the lock unit: a missing rules file must
# fail it, not skip it.
#
# What this does NOT do: stop another account from BINDING the port while the
# tunnel is down (nft has no say in bind). That is what require_auth in the backup
# job is for (README "Squatting"). It also does not stop a process that runs as
# root or as an allowed user.

TUNNEL_LOCK_UNIT="ikenga-tunnel-lock.service"
TUNNEL_LOCK_DIR="/etc/ikenga-tunnels"
TUNNEL_LOCK_FILE="$TUNNEL_LOCK_DIR/lock.nft"
TUNNEL_LOCK_TABLE="ikenga_tunnels"
TUNNEL_LOCK_DROPIN="10-ikenga-lock.conf"
# Defence in depth for the AF_SMC route (see above): the smc modules cannot be loaded.
# An unprivileged socket(AF_SMC) autoloads them on a stock Ubuntu kernel.
TUNNEL_NOSMC_FILE="/etc/modprobe.d/ikenga-no-smc.conf"
TL_UIDS=(0); TL_DESC="root(0)"; TL_PENDING=()

# Names -> uids. An allowed user that does not exist is an error (a dry run
# reports the same refusal as the real run), except the backup user while
# backups are enabled (the backups phase creates it, after this one, and the lock
# is converged again once it exists).
tunnel_lock_resolve() {
  local n line uid
  TL_UIDS=(0); TL_DESC="root(0)"; TL_PENDING=()
  for n in "${TN_ALLOW[@]}"; do
    if line="$(getent passwd "$n")"; then
      uid="$(cut -d: -f3 <<<"$line")"
      [[ "$uid" =~ ^[0-9]+$ ]] || die "TUNNEL_ALLOW_USERS: cannot read the uid of '$n'"
      (( uid > 0 )) || continue
      [[ " ${TL_UIDS[*]} " == *" $uid "* ]] && continue
      TL_UIDS+=("$uid"); TL_DESC+=", $n($uid)"
    elif [[ "$n" == "$BACKUP_USER" && "$BACKUPS_ENABLED" == 1 ]]; then
      TL_PENDING+=("$n")
    else
      die "TUNNEL_ALLOW_USERS names '$n', which is not a user on this host. Create it first, or remove it from the list"
    fi
  done
  mapfile -t TL_UIDS < <(printf '%s\n' "${TL_UIDS[@]}" | sort -n)
}

# The nft file for the current profile. Per tunnel port and family: accept the
# allowed uids, then reject everything else (the reject carries the counter).
tunnel_lock_rules() {
  local n p fam addr set ids
  set="$(printf '%s, ' "${TL_UIDS[@]}")"; set="${set%, }"
  ids="${set//[ ]/}"
  printf '%s\n' "# Managed by ikenga provision.sh (tunnels): change TUNNELS / TUNNEL_ALLOW_USERS in the profile, not this file." \
    "# Connections to a tunnel's local port are accepted from: ${TL_DESC}; everything else is rejected (so a socket without an owner is rejected too). Only this table is replaced; ufw's rules are not touched." \
    "table inet $TUNNEL_LOCK_TABLE" \
    "delete table inet $TUNNEL_LOCK_TABLE" \
    "table inet $TUNNEL_LOCK_TABLE {" \
    "  chain output {" \
    "    type filter hook output priority 0; policy accept;"
  for n in "${TN_NAMES[@]}"; do
    p="${TN_LPORT[$n]}"
    for fam in ip ip6; do
      if [[ $fam == ip ]]; then addr="127.0.0.1"; else addr="::1"; fi
      printf '    %s daddr %s tcp dport %s meta skuid { %s } accept comment "ikenga:lock:%s:%s:%s:allow:%s"\n' \
        "$fam" "$addr" "$p" "$set" "$n" "$fam" "$p" "$ids"
      printf '    %s daddr %s tcp dport %s counter reject with tcp reset comment "ikenga:lock:%s:%s:%s:deny"\n' \
        "$fam" "$addr" "$p" "$n" "$fam" "$p"
    done
  done
  printf '%s\n' "  }" "}"
}

# One line per rule, in order: "<verb> <comment>". Works on both our own text and
# on `nft list table` output, which words the same rule differently (counter
# values, set braces) but keeps the verb, the comment and the order.
tunnel_lock_signature() {
  awk '/^[[:space:]]*(#|table |delete table |chain |type |\}|$)/ { next }
       { v = "other"; if ($0 ~ / reject /) v = "reject"; else if ($0 ~ / accept( |$)/) v = "accept"
         c = ""; if (match($0, /ikenga:lock:[^"]+/)) c = substr($0, RSTART, RLENGTH)
         print v, c }'
}

# The smc modules cannot be loaded (see the head of this section).
tunnel_nosmc_text() {
  printf '%s\n' "# Managed by ikenga provision.sh (tunnels): the tunnel port lock cannot be bypassed through AF_SMC." \
    "# Removed again when the profile has no TUNNELS." \
    "install smc /bin/false" \
    "install smc_diag /bin/false"
}

# Written only when the lock is enabled. A module that is already loaded is not
# unloaded by this (it may be in use); the port rules do not depend on it.
sync_tunnel_nosmc() {
  if [[ $DRY_RUN -eq 0 && ! -d /etc/modprobe.d ]]; then install -d -m 0755 -o root -g root -- /etc/modprobe.d; fi
  if tl_write "$TUNNEL_NOSMC_FILE" 0644 "$(tunnel_nosmc_text)"; then
    changed "$TUNNEL_NOSMC_FILE written (the smc kernel modules cannot be loaded)"
  fi
  if [[ -d /sys/module/smc ]]; then
    note "the smc kernel module is already loaded; it is not unloaded automatically. Unload it ('rmmod smc_diag smc') or reboot; the port-lock rules hold either way"
  fi
}

# Refuse a symlink anywhere root is about to write the lock, before writing any of it.
tunnel_lock_refuse_symlinks() {
  local f
  for f in "$TUNNEL_LOCK_DIR" "$TUNNEL_LOCK_FILE" "$TUNNEL_NOSMC_FILE" "$SYSTEMD_DIR/$TUNNEL_LOCK_UNIT"; do
    [[ ! -L "$f" ]] || die "$f is a symbolic link. Refusing to write through it. Inspect it, remove it by hand, and run again."
  done
  [[ ! -e "$TUNNEL_LOCK_DIR" || -d "$TUNNEL_LOCK_DIR" ]] || die "$TUNNEL_LOCK_DIR exists and is not a directory; refusing to touch it"
}

tunnel_lock_unit_text() {
  local nft; nft="$(command -v nft 2>/dev/null || true)"; nft="${nft:-/usr/sbin/nft}"
  printf '%s\n' "# Managed by ikenga provision.sh (tunnels): change TUNNELS in the profile, not this file." \
    "[Unit]" \
    "Description=Ikenga: tunnel local ports reachable only by root and the allowed users (nftables table inet $TUNNEL_LOCK_TABLE)" \
    "Documentation=https://github.com/ikenga-hq/ikenga/blob/main/scripts/server/README.md" \
    "DefaultDependencies=no" \
    "After=local-fs.target" \
    "Before=network-pre.target shutdown.target" \
    "Wants=network-pre.target" \
    "Conflicts=shutdown.target" \
    "" \
    "[Service]" \
    "Type=oneshot" \
    "RemainAfterExit=yes" \
    "ExecStart=$nft -f $TUNNEL_LOCK_FILE" \
    "ExecReload=$nft -f $TUNNEL_LOCK_FILE" \
    "ExecStop=-$nft delete table inet $TUNNEL_LOCK_TABLE" \
    "" \
    "[Install]" \
    "WantedBy=sysinit.target"
}

tunnel_lock_dropin_text() {
  printf '%s\n' "# Managed by ikenga provision.sh (tunnels): the tunnel does not run without its port lock." \
    "[Unit]" \
    "Requires=$TUNNEL_LOCK_UNIT" \
    "After=$TUNNEL_LOCK_UNIT"
}

# tl_write <path> <mode> <content>: atomic write when the content or mode differs
# (the old file is copied aside). Everything here is root's, in root-owned
# directories. Returns 0 when it wrote.
tl_write() {
  local f="$1" mode="$2" want="$3" ts tmp
  [[ ! -L "$f" ]] || die "$f is a symbolic link. Refusing to write through it. Inspect it, remove it by hand, and run again."
  if [[ -e "$f" ]]; then
    [[ -f "$f" ]] || die "$f exists and is not a regular file; refusing to touch it"
    if [[ "$(cat -- "$f")" == "$want" ]]; then
      if [[ "$(stat -c '%a %u %g' -- "$f")" != "${mode#0} 0 0" ]]; then
        run chown 0:0 "$f"; run chmod "$mode" "$f"; changed "$f: owner/mode set to root $mode"
      fi
      return 1
    fi
    ts="$(date +%Y%m%d-%H%M%S)"
    if [[ $DRY_RUN -eq 0 ]]; then cp -p -- "$f" "$f.bak-$ts"; fi
    note "$f differs from the profile; the old one is kept as $(basename -- "$f").bak-$ts"
  fi
  if [[ $DRY_RUN -eq 1 ]]; then
    printf '    [dry-run] write %s:\n' "$f"
    printf '%s\n' "$want" | sed 's/^/      | /'
  else
    tmp="$(mktemp "$(dirname -- "$f")/.tmp.XXXXXX")"
    printf '%s\n' "$want" > "$tmp"; chown 0:0 "$tmp"; chmod "$mode" "$tmp"; mv -f -- "$tmp" "$f"
  fi
  return 0
}

# Is the live table exactly the wanted one? Compared rule by rule and in order by
# verb and comment (each comment names tunnel, family, port and, on an allow rule,
# the uids), so a deleted, edited, extra, reordered or stale-uid rule all read as
# drift. A reject that comes before its allow would lock everybody out.
tunnel_lock_live_ok() {   # rules-text
  local have got want
  command -v nft >/dev/null 2>&1 || return 1
  have="$(nft list table inet "$TUNNEL_LOCK_TABLE" 2>/dev/null)" || return 1
  got="$(tunnel_lock_signature <<<"$have")"
  want="$(tunnel_lock_signature <<<"$1")"
  [[ -n "$want" && "$got" == "$want" ]]
}

tunnel_lock_dropin_path() { printf '%s/%s.d/%s' "$SYSTEMD_DIR" "$(tunnel_unit_name "$1")" "$TUNNEL_LOCK_DROPIN"; }

tunnel_lock_remove() {
  local any=0 f lu="${SYSTEMD_DIR:?}/${TUNNEL_LOCK_UNIT:?}" lf="${TUNNEL_LOCK_FILE:?}"
  [[ ! -L "$TUNNEL_LOCK_DIR" ]] || die "$TUNNEL_LOCK_DIR is a symbolic link. Refusing to touch it."
  if [[ -e "$TUNNEL_NOSMC_FILE" || -L "$TUNNEL_NOSMC_FILE" ]]; then
    [[ ! -L "$TUNNEL_NOSMC_FILE" ]] || die "$TUNNEL_NOSMC_FILE is a symbolic link. Refusing to touch it."
    if [[ $DRY_RUN -eq 0 ]]; then rm -f -- "$TUNNEL_NOSMC_FILE"; fi
    changed "$TUNNEL_NOSMC_FILE removed"; any=1
  fi
  if [[ -e "$lu" || -L "$lu" ]]; then
    [[ ! -L "$lu" ]] || die "$lu is a symbolic link. Refusing to touch it."
    if [[ $DRY_RUN -eq 0 ]]; then sc disable --now "$TUNNEL_LOCK_UNIT" >/dev/null 2>&1 || true; rm -f -- "$lu"; fi
    changed "unit $TUNNEL_LOCK_UNIT stopped, disabled and removed (no tunnels in the profile)"; any=1
  fi
  if [[ -e "$lf" || -L "$lf" ]]; then
    [[ ! -L "$lf" ]] || die "$lf is a symbolic link. Refusing to touch it."
    if [[ $DRY_RUN -eq 0 ]]; then rm -f -- "$lf"; rmdir -- "${TUNNEL_LOCK_DIR:?}" 2>/dev/null || true; fi
    changed "$lf removed"; any=1
  fi
  for f in "${SYSTEMD_DIR:?}"/*-tunnel.service.d/"${TUNNEL_LOCK_DROPIN:?}"; do
    [[ -e "$f" || -L "$f" ]] || continue
    if [[ $DRY_RUN -eq 0 ]]; then rm -f -- "$f"; rmdir -- "$(dirname -- "$f")" 2>/dev/null || true; fi
    any=1
  done
  # The table is also gone from the kernel (ExecStop); belt and braces for a unit that was already removed.
  if [[ $DRY_RUN -eq 0 ]] && command -v nft >/dev/null 2>&1 && nft list table inet "$TUNNEL_LOCK_TABLE" >/dev/null 2>&1; then
    nft delete table inet "$TUNNEL_LOCK_TABLE" 2>/dev/null || true
  fi
  [[ $any -eq 0 || $DRY_RUN -eq 1 ]] || sc daemon-reload
  return 0
}

sync_tunnel_lock() {
  [[ $TUNNELS_DEFINED -eq 1 ]] || return 0
  if [[ ${#TN_NAMES[@]} -eq 0 ]]; then
    if [[ -e "$SYSTEMD_DIR/$TUNNEL_LOCK_UNIT" || -L "$SYSTEMD_DIR/$TUNNEL_LOCK_UNIT" || -e "$TUNNEL_LOCK_FILE" || -L "$TUNNEL_LOCK_FILE" || -L "$TUNNEL_LOCK_DIR" || -e "$TUNNEL_NOSMC_FILE" || -L "$TUNNEL_NOSMC_FILE" ]]; then
      [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo)"
      tunnel_lock_remove
    fi
    return 0
  fi
  [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo)"
  local n f st rules reload_unit=0 file_changed=0 unit_changed=0 tmp
  tunnel_lock_resolve
  tunnel_lock_refuse_symlinks
  [[ ${#TL_PENDING[@]} -eq 0 ]] || note "port lock: ${TL_PENDING[*]} does not exist yet; it is allowed once it does (the backups phase creates it, then this converges again)"
  backup_apt_install nftables
  rules="$(tunnel_lock_rules)"

  # Validation before change: the generated file must be accepted by nft itself.
  if [[ $DRY_RUN -eq 0 ]] && command -v nft >/dev/null 2>&1; then
    tmp="$(mktemp)"; printf '%s\n' "$rules" > "$tmp"
    nft -c -f "$tmp" >/dev/null 2>&1 || { rm -f -- "$tmp"; die "nft rejected the generated port-lock rules (nothing was changed); is the nf_tables kernel module available?"; }
    rm -f -- "$tmp"
  fi

  ensure_dir 0755 root root "$TUNNEL_LOCK_DIR"
  if tl_write "$TUNNEL_LOCK_FILE" 0644 "$rules"; then
    file_changed=1
    changed "$TUNNEL_LOCK_FILE written (ports: $(for n in "${TN_NAMES[@]}"; do printf '%s ' "${TN_LPORT[$n]}"; done)/ allowed: $TL_DESC)"
  fi
  if tl_write "$SYSTEMD_DIR/$TUNNEL_LOCK_UNIT" 0644 "$(tunnel_lock_unit_text)"; then
    unit_changed=1; reload_unit=1; changed "unit $TUNNEL_LOCK_UNIT installed"
  fi
  sync_tunnel_nosmc
  for n in "${TN_NAMES[@]}"; do
    f="$(tunnel_lock_dropin_path "$n")"
    if [[ $DRY_RUN -eq 0 ]]; then
      [[ ! -L "$(dirname -- "$f")" ]] || die "$(dirname -- "$f") is a symbolic link. Refusing to write through it."
      [[ -d "$(dirname -- "$f")" ]] || install -d -m 0755 -o root -g root -- "$(dirname -- "$f")"
    fi
    if tl_write "$f" 0644 "$(tunnel_lock_dropin_text)"; then
      reload_unit=1; changed "unit $(tunnel_unit_name "$n"): requires $TUNNEL_LOCK_UNIT (drop-in $TUNNEL_LOCK_DROPIN)"
    fi
  done
  if [[ $DRY_RUN -eq 1 ]]; then
    if [[ $file_changed -eq 1 || $unit_changed -eq 1 ]] || ! tunnel_lock_live_ok "$rules"; then
      note "[dry-run] systemctl daemon-reload; enable --now $TUNNEL_LOCK_UNIT (loads nft table inet $TUNNEL_LOCK_TABLE)"
    fi
    return 0
  fi
  [[ $reload_unit -eq 0 ]] || sc daemon-reload
  st="$(sc is-active "$TUNNEL_LOCK_UNIT" 2>/dev/null || true)"
  if ! sc is-enabled --quiet "$TUNNEL_LOCK_UNIT" 2>/dev/null || [[ "$st" != active ]]; then
    sc enable --now "$TUNNEL_LOCK_UNIT" >/dev/null 2>&1 || soft_fail "could not start $TUNNEL_LOCK_UNIT (see: systemctl status $TUNNEL_LOCK_UNIT). The tunnel units require it and will not run"
    changed "unit $TUNNEL_LOCK_UNIT enabled and started (port lock loaded)"
  elif [[ $file_changed -eq 1 ]] || ! tunnel_lock_live_ok "$rules"; then
    # reload = nft -f again: the table is replaced in one transaction, so there is no gap.
    sc reload "$TUNNEL_LOCK_UNIT" >/dev/null 2>&1 || soft_fail "could not reload $TUNNEL_LOCK_UNIT"
    changed "port lock reloaded ($([[ $file_changed -eq 1 ]] && echo 'rules changed' || echo 'the live table had drifted'))"
  fi
  tunnel_lock_live_ok "$rules" || soft_fail "the port lock is not live: nft list table inet $TUNNEL_LOCK_TABLE does not match $TUNNEL_LOCK_FILE"
  return 0
}

# ---- converge

sync_tunnels() {
  if [[ $TUNNELS_DEFINED -eq 0 ]]; then
    [[ "$ACTION" != tunnels ]] || note "nothing to do: the profile does not define TUNNELS (existing tunnel units are left alone)"
    return 0
  fi
  local stale n u st unit_reload=0 started=" "
  stale="$(tunnel_stale_units)"
  if [[ ${#TN_NAMES[@]} -eq 0 && -z "$stale" ]]; then
    # No tunnel left to lock: a lock left behind by a tunnel that is gone is removed too.
    sync_tunnel_lock
    [[ "$ACTION" != tunnels || ${#CHANGES[@]} -gt 0 ]] || note "nothing to do: TUNNELS is empty and no tunnel unit of $TUNNEL_USER exists"
    return 0
  fi
  log "SSH tunnels (user $TUNNEL_USER; ${#TN_NAMES[@]} tunnel(s))"
  [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo)"
  # A symlink where the lock is written is refused before anything (a stale tunnel included) changes.
  tunnel_lock_refuse_symlinks

  # Root-owned things go first, before any check the tunnel user can influence:
  # a symlink planted in its own tree must not keep a removed tunnel running.
  # Profile-only checks first: a malformed pinned host key must stop the run
  # before anything (including stale-unit removal) changes on the host.
  backup_apt_install openssh-client util-linux iproute2
  if [[ ${#TN_NAMES[@]} -gt 0 ]]; then tunnel_check_host_keys; fi
  # Who may connect is resolved (and an unknown TUNNEL_ALLOW_USERS name refused)
  # before anything is changed.
  if [[ ${#TN_NAMES[@]} -gt 0 ]]; then tunnel_lock_resolve; fi
  tunnel_remove_stale
  if [[ ${#TN_NAMES[@]} -eq 0 ]]; then sync_tunnel_lock; return 0; fi

  tunnel_refuse_symlinks
  tunnel_ensure_user
  backup_safe_dir 0700 "$TUNNEL_USER" "$TUNNEL_USER" "${TN_UID:-0}" "$TUNNEL_HOME"
  tunnel_load_ids || true
  tunnel_ensure_ssh_dir
  tunnel_ensure_key
  tunnel_sync_known_hosts

  # The port lock is in place (and the tunnels depend on it) before a tunnel starts.
  sync_tunnel_lock

  for n in "${TN_NAMES[@]}"; do
    if tunnel_write_unit "$n"; then unit_reload=1; fi
  done
  if [[ $DRY_RUN -eq 1 ]]; then
    note "[dry-run] systemctl daemon-reload; enable --now $(for n in "${TN_NAMES[@]}"; do printf '%s ' "$(tunnel_unit_name "$n")"; done)"
  else
    [[ $unit_reload -eq 0 ]] || sc daemon-reload
    for n in "${TN_NAMES[@]}"; do
      u="$(tunnel_unit_name "$n")"
      # "activating" is a tunnel waiting out RestartSec (the remote refused it,
      # or is down): systemd is doing its job, and a rerun must not touch it.
      st="$(sc is-active "$u" 2>/dev/null || true)"
      if ! sc is-enabled --quiet "$u" 2>/dev/null || [[ "$st" != active && "$st" != activating ]]; then
        sc enable --now "$u" >/dev/null 2>&1 || soft_fail "could not enable $u (see: systemctl status $u)"
        changed "unit $u enabled and started"; started+="$n "
      elif [[ "$TN_UCHANGED" == *" $n "* || $TN_KH_CHANGED -eq 1 ]]; then
        sc restart "$u" >/dev/null 2>&1 || soft_fail "could not restart $u"
        changed "unit $u restarted ($([[ "$TN_UCHANGED" == *" $n "* ]] && echo 'unit changed' || echo 'pinned host keys changed'))"; started+="$n "
      fi
    done
    # Is it connected? ssh opens the local port only after the remote accepted it.
    local i up
    for n in "${TN_NAMES[@]}"; do
      up=0
      for i in 1 2 3 4 5 6; do
        if ss -ltn 2>/dev/null | grep -qE "127\.0\.0\.1:${TN_LPORT[$n]}\s"; then up=1; break; fi
        [[ "$started" == *" $n "* ]] || break
        sleep 1
      done
      if [[ $up -eq 1 ]]; then note "$n: 127.0.0.1:${TN_LPORT[$n]} is listening"
      else note "$n: 127.0.0.1:${TN_LPORT[$n]} is not listening yet (not connected: install the authorized_keys line below on the remote host; the unit retries every 10 s)"; fi
    done
  fi
  tunnel_print_remote_lines
}

# ------------------------------------------------------------------- swap

# A swap file for a small box (a 3.8 GB host with three ~280 MB agent sessions
# each runs out of memory without one). SWAP_SIZE / SWAPPINESS / SWAP_FILE; see
# README "Swap". The rule that matters: if ANY swap is already active that we
# did not create, we leave swap alone and say so.
#
# What we own, and only this: SWAP_FILE, one fstab line ending in
# "# ikenga-swap", and /etc/sysctl.d/90-ikenga-swap.conf. The fstab line is the
# ownership marker: a swap file without it is never touched.
SWAP_MARK="# ikenga-swap"
SWAP_SYSCTL=/etc/sysctl.d/90-ikenga-swap.conf
SWAP_FSTAB="${IKENGA_FSTAB:-/etc/fstab}"
SWAP_MEMINFO="${IKENGA_MEMINFO:-/proc/meminfo}"
SWAP_MARGIN_MB=256     # memory that must stay free after swapoff pulls the used swap back in
SWAP_MB=0              # resolved size in MiB; 0 = off

validate_swap_profile() {
  SWAP_SIZE="${SWAP_SIZE,,}"
  [[ "$SWAP_SIZE" =~ ^(auto|off|0|[0-9]+[mg])$ ]] || die "SWAP_SIZE must be auto, off, 0, or a size like 2G or 4096M (got '$SWAP_SIZE')"
  [[ "$SWAPPINESS" =~ ^[0-9]{1,3}$ ]] && (( 10#$SWAPPINESS <= 100 )) || die "SWAPPINESS must be a number from 0 to 100 (got '$SWAPPINESS')"
  SWAPPINESS=$((10#$SWAPPINESS))
  [[ "$SWAP_FILE" =~ ^/[A-Za-z0-9._/-]+$ && "$SWAP_FILE" != */ && "$SWAP_FILE" != *..* && "$SWAP_FILE" != *//* ]] \
    || die "SWAP_FILE must be a plain absolute path of letters, digits and . _ - / (got '$SWAP_FILE')"
  case "$SWAP_SIZE" in
    off|0) SWAP_MB=0 ;;
    auto)
      local kb gb; kb="$(awk '/^MemTotal:/{print $2}' "$SWAP_MEMINFO" 2>/dev/null || true)"
      [[ "$kb" =~ ^[0-9]+$ ]] || die "cannot read MemTotal from $SWAP_MEMINFO to size SWAP_SIZE=auto"
      gb=$(( (kb + 1048575) / 1048576 ))      # RAM in GiB, rounded up (a "4 GB" box reports a little under 4)
      if (( gb <= 8 )); then SWAP_MB=4096; else SWAP_MB=2048; fi ;;
    *)
      SWAP_MB=$(( 10#${SWAP_SIZE%[mg]} ))
      [[ "$SWAP_SIZE" == *g ]] && SWAP_MB=$(( SWAP_MB * 1024 ))
      (( SWAP_MB >= 64 && SWAP_MB <= 65536 )) || die "SWAP_SIZE '$SWAP_SIZE' is out of range (64M to 64G)" ;;
  esac
}

# Active swap names, one per line (empty when none or when swapon is missing).
swap_active() { command -v swapon >/dev/null 2>&1 && swapon --noheadings --raw --show=NAME 2>/dev/null || true; }
swap_is_active() { swap_active | grep -qxF -- "$1"; }
swap_used_bytes() { swapon --noheadings --raw --bytes --show=NAME,USED 2>/dev/null | awk -v n="$1" '$1==n{print $2; exit}'; }
swap_mem_avail_kb() { awk '/^MemAvailable:/{print $2}' "$SWAP_MEMINFO" 2>/dev/null; }

# The path in the managed fstab line, if there is one (first one; a dup is repaired later).
swap_marked_path() { awk '$NF=="ikenga-swap" && $(NF-1)=="#" {print $1; exit}' "$SWAP_FSTAB" 2>/dev/null || true; }
swap_mark_count() { grep -cE '[[:space:]]#[[:space:]]ikenga-swap[[:space:]]*$' "$SWAP_FSTAB" 2>/dev/null || true; }

# swap_fstab_set <line|"">: make fstab hold exactly this one managed line (or none).
# Backs the file up before changing it. Returns 1 when nothing needed changing.
swap_fstab_set() {
  local want="$1" n cur
  n="$(swap_mark_count)"; n="${n:-0}"
  cur="$(grep -E '[[:space:]]#[[:space:]]ikenga-swap[[:space:]]*$' "$SWAP_FSTAB" 2>/dev/null || true)"
  if [[ -z "$want" ]]; then [[ "$n" -gt 0 ]] || return 1
  elif [[ "$n" -eq 1 && "$cur" == "$want" ]]; then return 1
  fi
  [[ $DRY_RUN -eq 1 ]] && return 0
  local tmp; tmp="$(mktemp)"
  { [[ -f "$SWAP_FSTAB" ]] && grep -vE '[[:space:]]#[[:space:]]ikenga-swap[[:space:]]*$' "$SWAP_FSTAB"
    [[ -z "$want" ]] || printf '%s\n' "$want"
    true
  } > "$tmp"
  if [[ -f "$SWAP_FSTAB" ]]; then
    cp -a "$SWAP_FSTAB" "$SWAP_FSTAB.bak-$(date +%Y%m%d-%H%M%S)"
    find "$(dirname -- "$SWAP_FSTAB")" -maxdepth 1 -name "$(basename -- "$SWAP_FSTAB").bak-*" | sort | head -n -5 | xargs -r rm -f --
    cat "$tmp" > "$SWAP_FSTAB"
  else
    install -m 0644 -o root -g root "$tmp" "$SWAP_FSTAB"
  fi
  rm -f "$tmp"
  return 0
}

# Is it safe to turn this swap off? Everything it holds has to fit back in RAM.
swap_can_swapoff() {   # path
  local used avail
  used="$(swap_used_bytes "$1")"; used="${used:-0}"
  (( used > 0 )) || return 0
  avail="$(swap_mem_avail_kb)"; avail="${avail:-0}"
  if (( used + SWAP_MARGIN_MB * 1048576 > avail * 1024 )); then
    soft_fail "swap: refusing to turn off $1: $((used / 1048576)) MiB is in use and only $((avail / 1024)) MiB of memory is available (need that plus ${SWAP_MARGIN_MB} MiB spare). Nothing was changed; retry when the box is quieter."
    return 1
  fi
}

# Checks on SWAP_FILE that must hold before we create or replace anything.
swap_check_target() {
  local f="$SWAP_FILE" dir fs
  dir="$(dirname -- "$f")"
  [[ ! -L "$f" ]] || { soft_fail "swap: $f is a symlink; refusing (point SWAP_FILE at a real path)"; return 1; }
  [[ -d "$dir" ]] || { soft_fail "swap: directory $dir does not exist"; return 1; }
  [[ "$(realpath -m -- "$f")" == "$f" ]] || { soft_fail "swap: $f goes through a symlink (resolves to $(realpath -m -- "$f")); refusing"; return 1; }
  [[ ! -e "$f" || -f "$f" ]] || { soft_fail "swap: $f exists and is not a regular file"; return 1; }
  fs="$(findmnt -n -o FSTYPE -T "$dir" 2>/dev/null | head -1 || true)"
  [[ -n "$fs" ]] || fs="$(stat -f -c %T "$dir" 2>/dev/null || true)"
  case "$fs" in
    ext2|ext3|ext4|ext2/ext3|xfs|f2fs) ;;
    btrfs) soft_fail "swap: $dir is on btrfs, which needs a nodatacow swap file made by 'btrfs filesystem mkswapfile'; not supported here. Put SWAP_FILE on an ext4 or xfs filesystem."; return 1 ;;
    *) soft_fail "swap: $dir is on '${fs:-unknown}', not a local ext4/xfs/f2fs filesystem; a swap file cannot live there. Choose another SWAP_FILE."; return 1 ;;
  esac
}

# Keep at least 10% of the filesystem free after the file is in place.
# credit_mb = space that frees up first (the file being replaced).
swap_disk_ok() {   # need_mb credit_mb
  local dir free_kb total_kb
  dir="$(dirname -- "$SWAP_FILE")"
  read -r total_kb free_kb < <(df -Pk -- "$dir" | awk 'NR==2{print $2, $4}')
  if (( (free_kb + $2 * 1024 - $1 * 1024) * 10 < total_kb )); then
    soft_fail "swap: not enough free disk on $dir for a $1 MiB swap file ($((free_kb / 1024)) MiB free of $((total_kb / 1024)) MiB; at least 10% must stay free). Choose a smaller SWAP_SIZE."
    return 1
  fi
}

swap_signature_ok() {
  command -v blkid >/dev/null 2>&1 || return 0
  [[ "$(blkid -p -o value -s TYPE -- "$1" 2>/dev/null || true)" == swap ]]
}

# Build the file next to its final name, then rename it into place: SWAP_FILE
# is never a half-written file.
swap_create() {   # mode: fallocate|dd
  local f="$SWAP_FILE" tmp="$SWAP_FILE.ikenga-new" mode="$1"
  run rm -f -- "$tmp"
  run install -m 0600 -o root -g root /dev/null "$tmp"
  if [[ "$mode" == fallocate ]]; then
    if ! run fallocate -l "${SWAP_MB}M" -- "$tmp"; then
      note "fallocate refused on this filesystem; falling back to dd"; mode=dd
      run truncate -s 0 -- "$tmp"
    fi
  fi
  if [[ "$mode" == dd ]]; then
    run dd if=/dev/zero of="$tmp" bs=1M count="$SWAP_MB" status=none || { rm -f -- "$tmp"; soft_fail "swap: could not write $SWAP_MB MiB to $tmp"; return 1; }
  fi
  run chmod 0600 -- "$tmp"
  run mkswap -q -- "$tmp" || { rm -f -- "$tmp"; soft_fail "swap: mkswap failed on $tmp"; return 1; }
  run mv -f -- "$tmp" "$f"
}

# Create the file, record it in fstab, turn it on. Retries once with dd when
# fallocate left a file the kernel will not swap on (holes).
swap_make_active() {
  local f="$SWAP_FILE" line="$SWAP_FILE none swap sw,nofail 0 0 $SWAP_MARK"
  swap_create fallocate || return 1
  if swap_fstab_set "$line"; then changed "swap: fstab line added for $f"; fi
  if [[ $DRY_RUN -eq 1 ]]; then run swapon "$f"; return 0; fi
  if ! swapon "$f" 2>/dev/null; then
    note "swapon refused the fallocate'd file; rebuilding it with dd"
    swapoff "$f" 2>/dev/null || true; rm -f -- "$f"
    swap_create dd || return 1
    if ! swapon "$f"; then
      rm -f -- "$f"; swap_fstab_set "" >/dev/null || true
      soft_fail "swap: swapon failed on $f (a container or a filesystem without swap support?). The file and its fstab line were removed."
      return 1
    fi
  fi
}

swap_sysctl() {
  local want="# ikenga: managed by provision.sh swap
vm.swappiness = $SWAPPINESS" live
  live="$(cat /proc/sys/vm/swappiness 2>/dev/null || true)"
  if [[ "$(cat "$SWAP_SYSCTL" 2>/dev/null || true)" != "$want" ]]; then
    if [[ -f "$SWAP_SYSCTL" ]]; then run cp -a "$SWAP_SYSCTL" "$SWAP_SYSCTL.bak-$(date +%Y%m%d-%H%M%S)"; fi
    run install -d -m 0755 "$(dirname -- "$SWAP_SYSCTL")"
    if [[ $DRY_RUN -eq 1 ]]; then note "[dry-run] write $SWAP_SYSCTL (vm.swappiness = $SWAPPINESS)"
    else printf '%s\n' "$want" > "$SWAP_SYSCTL"; chmod 0644 "$SWAP_SYSCTL"; fi
    changed "swap: vm.swappiness = $SWAPPINESS ($SWAP_SYSCTL)"
    run sysctl -q -p "$SWAP_SYSCTL" || soft_fail "swap: sysctl could not apply $SWAP_SYSCTL (it takes effect at the next boot)"
  elif [[ -n "$live" && "$live" != "$SWAPPINESS" ]]; then
    run sysctl -q -p "$SWAP_SYSCTL" || soft_fail "swap: sysctl could not apply $SWAP_SYSCTL"
    changed "swap: vm.swappiness $live -> $SWAPPINESS (re-applied $SWAP_SYSCTL)"
  fi
}

# SWAP_SIZE=off: take away only what we made.
swap_remove() {
  local mp; mp="$(swap_marked_path)"
  if [[ -z "$mp" && ! -f "$SWAP_SYSCTL" ]]; then note "swap: off, and none is managed here; nothing to remove"; return 0; fi
  if [[ -n "$mp" ]] && swap_is_active "$mp"; then
    swap_can_swapoff "$mp" || return 1
    run swapoff -- "$mp" || { soft_fail "swap: swapoff $mp failed"; return 1; }
    changed "swap: $mp turned off"
  fi
  if swap_fstab_set ""; then changed "swap: fstab line removed"; fi
  if [[ -n "$mp" && ( -f "$mp" || -L "$mp" ) ]]; then
    if [[ -L "$mp" ]]; then soft_fail "swap: $mp is a symlink; left in place"
    else run rm -f -- "$mp"; changed "swap: $mp removed"; fi
  fi
  if [[ -f "$SWAP_SYSCTL" ]]; then
    run rm -f -- "$SWAP_SYSCTL"; changed "swap: $SWAP_SYSCTL removed (the live vm.swappiness stays until reboot)"
  fi
}

sync_swap() {
  log "Swap"
  if (( SWAP_MB == 0 )); then swap_remove; return 0; fi
  local f="$SWAP_FILE" mp act others want_bytes have
  mp="$(swap_marked_path)"
  act="$(swap_active)"
  others="$(printf '%s\n' "$act" | grep -vxF -- "${mp:-}" | grep -v '^$' || true)"
  if [[ -n "$others" ]]; then
    note "swap: other swap is already active, leaving swap alone ($(printf '%s' "$others" | paste -sd, -)):"
    note "      set SWAP_SIZE=off to stop managing swap here; the sizes and settings above are not applied"
    return 0
  fi
  if [[ -n "$mp" && "$mp" != "$f" ]]; then
    soft_fail "swap: the managed swap file is $mp but SWAP_FILE is $f. Set SWAP_FILE=$mp, or SWAP_SIZE=off to remove it first."
    return 1
  fi
  swap_check_target || return 1

  want_bytes=$(( SWAP_MB * 1048576 ))
  rm -f -- "$f.ikenga-new" 2>/dev/null || true   # a run that died mid-build
  if [[ -e "$f" ]]; then
    if [[ -z "$mp" ]]; then
      soft_fail "swap: $f already exists and is not managed by ikenga (no '$SWAP_MARK' line in $SWAP_FSTAB). Move it, or point SWAP_FILE elsewhere."
      return 1
    fi
    have="$(stat -c %s -- "$f")"
    if [[ "$have" == "$want_bytes" ]] && swap_signature_ok "$f"; then
      local mode owner; read -r mode owner < <(stat -c '%a %u:%g' -- "$f")
      if [[ "$mode" != 600 || "$owner" != 0:0 ]]; then run chown root:root -- "$f"; run chmod 0600 -- "$f"; changed "swap: $f permissions set to root:root 0600"; fi
      if swap_fstab_set "$f none swap sw,nofail 0 0 $SWAP_MARK"; then changed "swap: fstab line repaired"; fi
      if ! swap_is_active "$f"; then
        if run swapon "$f"; then changed "swap: $f turned on"; else soft_fail "swap: swapon $f failed"; return 1; fi
      fi
    else
      # Resize (or a file that is not valid swap): off, replace, on.
      swap_disk_ok "$SWAP_MB" "$(( have / 1048576 ))" || return 1
      if swap_is_active "$f"; then
        swap_can_swapoff "$f" || return 1
        run swapoff -- "$f" || { soft_fail "swap: swapoff $f failed"; return 1; }
      fi
      run rm -f -- "$f"
      swap_make_active || return 1
      changed "swap: $f resized $(( have / 1048576 )) MiB -> $SWAP_MB MiB"
    fi
  else
    # An unmanaged fstab entry for this path would be started twice at boot.
    if grep -E "^[[:space:]]*$(printf '%s' "$f" | sed 's/[.[\*^$/]/\\&/g')[[:space:]]+[^#]*swap" "$SWAP_FSTAB" 2>/dev/null | grep -qvE '#[[:space:]]ikenga-swap'; then
      soft_fail "swap: $SWAP_FSTAB already has a swap entry for $f that ikenga did not write. Remove it, or point SWAP_FILE elsewhere."
      return 1
    fi
    swap_disk_ok "$SWAP_MB" 0 || return 1
    swap_make_active || return 1
    changed "swap: $f created ($SWAP_MB MiB), turned on, and added to fstab"
  fi
  swap_sysctl
}

# ------------------------------------------------------------------ main

case "$ACTION" in
  upgrade)
    if [[ $DRY_RUN -eq 0 ]]; then
      take_upgrade_lock || die "another upgrade is running"
    fi
    do_upgrade
    exit 0 ;;
  check-update) do_check_update; exit 0 ;;
  apply-request) do_apply_request; exit 0 ;;
  install-update-units) install_update_units; summary; exit 0 ;;
  install-agent-cli-updates)
    [[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" ]] || die "install-agent-cli-updates needs a profile: pass --profile <file> (or provision once so $INSTALL_DIR/.profile.env exists)"
    validate_profile; install_agent_cli_updates; summary; exit 0 ;;
  sync-accounts)
    [[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" ]] || die "sync-accounts needs a profile: pass --profile <file> (or provision once so $INSTALL_DIR/.profile.env exists)"
    validate_accounts_profile
    sync_accounts
    summary
    [[ $FAILED -eq 0 ]] || { echo "error: some account sync steps failed; see the warnings above" >&2; exit 1; }
    exit 0 ;;
  tunnels)
    [[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" ]] || die "tunnels needs a profile: pass --profile <file> (or provision once so $INSTALL_DIR/.profile.env exists)"
    validate_tunnels_profile
    sync_tunnels
    summary
    [[ $FAILED -eq 0 ]] || { echo "error: some tunnel steps failed; see the warnings above" >&2; exit 1; }
    exit 0 ;;
  swap)
    [[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" ]] || die "swap needs a profile: pass --profile <file> (or provision once so $INSTALL_DIR/.profile.env exists)"
    validate_swap_profile
    sync_swap
    summary
    [[ $FAILED -eq 0 ]] || { echo "error: the swap step failed; see the warnings above" >&2; exit 1; }
    exit 0 ;;
  backups)
    [[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" ]] || die "backups needs a profile: pass --profile <file> (or provision once so $INSTALL_DIR/.profile.env exists)"
    validate_accounts_profile
    validate_backups_profile
    validate_tunnels_profile
    sync_backups
    # The backup user is allowed through the tunnel port lock; it exists now.
    sync_tunnel_lock
    summary
    [[ $FAILED -eq 0 ]] || { echo "error: some backup steps failed; see the warnings above" >&2; exit 1; }
    exit 0 ;;
esac

validate_profile
IKENGA_HOST_VALUE="127.0.0.1"; IKENGA_PUBLIC_URL_VALUE=""; ARCH=""; TS_IP=""; BINARY_CHANGED=0; ENV_CHANGED=0
preflight
confirm
sync_swap
harden_base
if [[ "$PERIMETER" == tailnet ]]; then perimeter_tailnet; else perimeter_public; fi
install_deps
install_daemon
write_env
if [[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" && $DRY_RUN -eq 0 ]]; then
  # Root sources this file unattended (the update units), so it is stored
  # root-owned and private, never with the operator's uid and mode.
  install -m 0600 -o root -g root "$PROFILE_FILE" "$INSTALL_DIR/.profile.env"
fi
install_service
install_update_units
install_agent_cli_updates
firewall
verify
sync_accounts
sync_tunnels
sync_backups
sync_tunnel_lock
summary
[[ $FAILED -eq 0 ]] || { echo "error: some swap, account, tunnel or backup steps failed; see the warnings above" >&2; exit 1; }
