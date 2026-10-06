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

if [[ "${1:-}" == "upgrade" ]]; then
  ACTION="upgrade"
  shift
fi

while [[ $# -gt 0 ]]; do
  case "$1" in
    upgrade) ACTION="upgrade"; shift ;;
    --to) TARGET_VERSION="${2:?--to needs a version}"; shift 2 ;;
    --latest) UPGRADE_LATEST=1; shift ;;
    --force) FORCE=1; shift ;;
    --profile) PROFILE_FILE="${2:?--profile needs a file}"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
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
        exit 0
      fi
      sed -n '3,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [[ "$ACTION" == "provision" ]]; then
  [[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" ]] || { echo "error: --profile <file> is required and must exist" >&2; exit 2; }
else
  if [[ -z "$PROFILE_FILE" && -f "$INSTALL_DIR/.profile.env" ]]; then
    PROFILE_FILE="$INSTALL_DIR/.profile.env"
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

if [[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$PROFILE_FILE"
elif [[ -f "$ENV_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$ENV_FILE" 2>/dev/null || true
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
  if [[ -z "$ver" ]]; then
    ver="$(curl -fsSI "https://github.com/$REPO/releases/latest" 2>/dev/null | grep -i '^location:' | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
  fi
  if [[ -z "$ver" ]]; then
    ver="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null | grep -oE '"tag_name":\s*"v?[0-9]+\.[0-9]+\.[0-9]+"' | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
  fi
  echo "$ver"
}

parse_manifest() {
  local manifest_file="$1"
  local arch="$2"

  if command -v jq >/dev/null 2>&1; then
    SCHEMA="$(jq -r '.schema // empty' "$manifest_file")"
    MANIFEST_VERSION="$(jq -r '.version // empty' "$manifest_file")"
    MANIFEST_CHANNEL="$(jq -r '.channel // empty' "$manifest_file")"
    MIN_UPGRADE_FROM="$(jq -r '.min_upgrade_from // empty' "$manifest_file")"
    ARTIFACT_NAME="$(jq -r --arg a "$arch" '.artifacts[] | select(.arch == $a and .kind == "tarball") | .name' "$manifest_file" | head -1)"
    ARTIFACT_SHA="$(jq -r --arg a "$arch" '.artifacts[] | select(.arch == $a and .kind == "tarball") | .sha256' "$manifest_file" | head -1)"
    ARTIFACT_SIZE="$(jq -r --arg a "$arch" '.artifacts[] | select(.arch == $a and .kind == "tarball") | .size' "$manifest_file" | head -1)"
  elif command -v python3 >/dev/null 2>&1; then
    local out
    out="$(python3 -c '
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
print("SCHEMA=" + json.dumps(str(d.get("schema", ""))))
print("MANIFEST_VERSION=" + json.dumps(str(d.get("version", ""))))
print("MANIFEST_CHANNEL=" + json.dumps(str(d.get("channel", ""))))
print("MIN_UPGRADE_FROM=" + json.dumps(str(d.get("min_upgrade_from", ""))))
arch = sys.argv[2]
art = next((a for a in d.get("artifacts", []) if a.get("arch") == arch and a.get("kind") == "tarball"), {})
print("ARTIFACT_NAME=" + json.dumps(str(art.get("name", ""))))
print("ARTIFACT_SHA=" + json.dumps(str(art.get("sha256", ""))))
print("ARTIFACT_SIZE=" + json.dumps(str(art.get("size", ""))))
' "$manifest_file" "$arch")"
    eval "$out"
  elif command -v node >/dev/null 2>&1; then
    local out
    out="$(node -e '
const fs = require("fs");
const d = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
console.log(`SCHEMA=${d.schema || ""}`);
console.log(`MANIFEST_VERSION=${d.version || ""}`);
console.log(`MANIFEST_CHANNEL=${d.channel || ""}`);
console.log(`MIN_UPGRADE_FROM=${d.min_upgrade_from || ""}`);
const arch = process.argv[2];
const art = (d.artifacts || []).find(a => a.arch === arch && a.kind === "tarball") || {};
console.log(`ARTIFACT_NAME=${art.name || ""}`);
console.log(`ARTIFACT_SHA=${art.sha256 || ""}`);
console.log(`ARTIFACT_SIZE=${art.size || ""}`);
' "$manifest_file" "$arch")"
    eval "$out"
  else
    die "jq, python3, or node is required to parse release manifest"
  fi
}

do_upgrade() {
  log "Upgrade ikenga-server"
  [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo); dry runs may be unprivileged"

  detect_arch

  local have=""
  if [[ -x "$INSTALL_DIR/bin/ikenga-server" ]]; then
    have="$("$INSTALL_DIR/bin/ikenga-server" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || true)"
  fi
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

  local SCHEMA="" MANIFEST_VERSION="" MANIFEST_CHANNEL="" MIN_UPGRADE_FROM=""
  local ARTIFACT_NAME="" ARTIFACT_SHA="" ARTIFACT_SIZE=""
  parse_manifest "$manifest_file" "$ARCH"

  [[ "$SCHEMA" == "ikenga-server-release/1" ]] || die "manifest has unsupported schema '$SCHEMA' (expected ikenga-server-release/1)"
  [[ -n "$MANIFEST_VERSION" ]] || die "manifest is missing version field"
  target="$MANIFEST_VERSION"

  if [[ -n "$MANIFEST_CHANNEL" && "$MANIFEST_CHANNEL" != "$channel" ]]; then
    die "manifest channel '$MANIFEST_CHANNEL' does not match profile channel '$channel'"
  fi

  if [[ "$have" == "$target" ]]; then
    note "already on version $target ($channel channel); nothing to upgrade"
    return 0
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
  rm -f "/etc/systemd/system/$unit.prev-$have"
  if [[ -f "$tmp/stage/$unit" ]]; then
    if [[ -f "/etc/systemd/system/$unit" ]]; then
      cp -a "/etc/systemd/system/$unit" "/etc/systemd/system/$unit.prev-$have"
      unit_saved=1
      note "previous unit saved as /etc/systemd/system/$unit.prev-$have (rollback guard)"
    fi
    install -m 0644 "$tmp/stage/$unit" "/etc/systemd/system/$unit"
    command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload 2>/dev/null || true
  fi

  log "Restarting $svc..."
  if command -v systemctl >/dev/null 2>&1; then
    systemctl restart "$svc" 2>/dev/null || true
  fi

  log "Checking health at /api/health..."
  local url="http://127.0.0.1:4000"
  if [[ "${PERIMETER:-}" == tailnet && -n "${IKENGA_HOST_VALUE:-}" ]]; then
    url="http://$IKENGA_HOST_VALUE:4000"
  fi

  local healthy=0
  for i in $(seq 1 "${HEALTH_TIMEOUT:-20}"); do
    if curl -fsS -m 2 "$url/api/health" 2>/dev/null | grep -q '"ok":\s*true'; then
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
  if [[ $unit_saved -eq 1 && -f "/etc/systemd/system/$unit.prev-$have" ]]; then
    cp -a "/etc/systemd/system/$unit.prev-$have" "/etc/systemd/system/$unit"
    command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload 2>/dev/null || true
  fi
  if command -v systemctl >/dev/null 2>&1; then
    systemctl restart "$svc" 2>/dev/null || true
  fi

  local rolled_back=0
  for i in $(seq 1 "${HEALTH_TIMEOUT:-20}"); do
    if curl -fsS -m 2 "$url/api/health" 2>/dev/null | grep -q '"ok":\s*true'; then
      rolled_back=1
      break
    fi
    sleep 1
  done

  if [[ $rolled_back -eq 1 ]]; then
    die "upgrade to $target failed health check; automatically rolled back to $have successfully"
  else
    die "upgrade to $target failed health check AND rollback to $have also failed; see journalctl -u $svc"
  fi
}

# ------------------------------------------------------------------ main

if [[ "$ACTION" == "upgrade" ]]; then
  do_upgrade
  exit 0
fi

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
  cp -a "$PROFILE_FILE" "$INSTALL_DIR/.profile.env" 2>/dev/null || true
fi
install_service
firewall
verify
summary
