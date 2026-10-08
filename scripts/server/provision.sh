#!/bin/bash
set -euo pipefail

# Provision a fresh Debian/Ubuntu host into a hardened, running ikenga-server.
#
#   provision.sh --profile <file> [--dry-run] [--yes] [--skip-hardening]
#   provision.sh --profile profiles/dixtrit-public.env --dry-run
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
  upgrade|check-update|apply-request|install-update-units|sync-accounts) ACTION="$1"; shift ;;
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
        printf 'Usage: %s check-update | apply-request | install-update-units | sync-accounts [--profile <file>] [--dry-run]\n\n' "${BASH_SOURCE[0]}"
        printf '  check-update          read the release manifest and write %s/available.json (installs nothing)\n' "$STATE_DIR"
        printf '  apply-request         claim and apply an admin update request (run by ikenga-update.service)\n'
        printf '  install-update-units  install %s and the update timer, path and service units\n' "$STABLE_COPY"
        printf '  sync-accounts         converge shared project mirrors, per-account clones and scoped secrets\n'
        printf '                        (run it after creating or removing accounts; the full provision run does it too)\n'
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

is_reserved_scope() { [[ "$1" == everyone || "$1" == agents || "$1" == root ]]; }

validate_accounts_profile() {
  local a e name rest
  for a in "${ACCOUNTS[@]}" "${AGENT_ACCOUNTS[@]}"; do
    [[ "$a" =~ ^[a-z][a-z0-9_-]{0,30}$ ]] || die "account '$a' is not a valid login name (lowercase letters, digits, - and _; the unix user is ik-<name>)"
    is_reserved_scope "$a" && die "account name '$a' is reserved (everyone, agents and root are secret scopes)"
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

declare -A ROOT_VALUE=() ROOT_COUNT=() ROOT_SCOPE=() SEC_BODY=() SEC_SEEN=()
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
  SEC_BODY=(); SEC_SEEN=(); ROOT_VALUE=(); ROOT_COUNT=(); ROOT_SCOPE=(); SECRETS_UNREADABLE=0
  [[ -n "$SECRETS_FILE" ]] || return 0
  if [[ ! -r "$SECRETS_FILE" ]]; then
    [[ $DRY_RUN -eq 1 ]] || die "cannot read SECRETS_FILE $SECRETS_FILE"
    SECRETS_UNREADABLE=1; note "WARNING: $SECRETS_FILE is not readable by this user; the secrets plan is skipped (run the dry run as root)"
    return 0
  fi
  secrets_file_ok "$SECRETS_FILE"
  local re='^\[([^]]+)\][[:space:]]+([A-Za-z_][A-Za-z0-9_]*)=(.*)$'
  local n=0 line scope name value tok a toks targets declared
  while IFS= read -r line || [[ -n "$line" ]]; do
    n=$((n+1))
    [[ "$line" =~ ^[[:space:]]*(#.*)?$ ]] && continue
    # Never echo the line: it holds the value.
    [[ "$line" =~ $re ]] || die "$SECRETS_FILE line $n: expected '[scope] NAME=value'"
    scope="${BASH_REMATCH[1]}"; name="${BASH_REMATCH[2]}"; value="${BASH_REMATCH[3]}"
    secret_name_ok "$name" || die "$SECRETS_FILE line $n: '$name' is not an allowed secret name (reserved or shell-sensitive)"
    [[ -n "$value" ]] || die "$SECRETS_FILE line $n: $name has an empty value"
    [[ "$value" != *$'\r'* ]] || die "$SECRETS_FILE line $n: carriage return in the value of $name (CRLF file?)"

    targets=" "; declared=" "
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
        *)
          [[ " ${ACCT_LOGINS[*]} " == *" $tok "* ]] || die "$SECRETS_FILE line $n: scope names '$tok', which is not a managed account (typo? or add it to ACCOUNTS)"
          [[ "$targets" == *" $tok "* ]] || targets+="$tok " ;;
      esac
    done
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
  sync-accounts)
    [[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" ]] || die "sync-accounts needs a profile: pass --profile <file> (or provision once so $INSTALL_DIR/.profile.env exists)"
    validate_accounts_profile
    sync_accounts
    summary
    [[ $FAILED -eq 0 ]] || { echo "error: some account sync steps failed; see the warnings above" >&2; exit 1; }
    exit 0 ;;
esac

validate_profile
IKENGA_HOST_VALUE="127.0.0.1"; IKENGA_PUBLIC_URL_VALUE=""; ARCH=""; TS_IP=""; BINARY_CHANGED=0; ENV_CHANGED=0
preflight
confirm
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
firewall
verify
sync_accounts
summary
[[ $FAILED -eq 0 ]] || { echo "error: some account sync steps failed; see the warnings above" >&2; exit 1; }
