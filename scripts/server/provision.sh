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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_DIR="${INSTALL_DIR:-/opt/ikenga}"
ENV_FILE="$INSTALL_DIR/.env"
REPO="ikenga-hq/ikenga"

DRY_RUN=0
ASSUME_YES=0
SKIP_HARDENING=0
PROFILE_FILE=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --profile) PROFILE_FILE="${2:?--profile needs a file}"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --yes|-y) ASSUME_YES=1; shift ;;
    --skip-hardening) SKIP_HARDENING=1; shift ;;
    -h|--help) sed -n '3,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
done

[[ -n "$PROFILE_FILE" && -f "$PROFILE_FILE" ]] || { echo "error: --profile <file> is required and must exist" >&2; exit 2; }

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
APPS=()
LIBS=()
AGENT_CLIS=()
FS_ROOTS=()
SECRETS_FROM=""

# shellcheck disable=SC1090
source "$PROFILE_FILE"

validate_profile() {
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
    [[ -n "$ACME_EMAIL" ]] || die "PERIMETER=public-https requires ACME_EMAIL"
  else
    [[ -n "$TS_AUTHKEY_FILE" ]] || die "PERIMETER=tailnet requires TS_AUTHKEY_FILE (a file holding a Tailscale auth key; never pass it in argv)"
    [[ -f "$TS_AUTHKEY_FILE" ]] || die "TS_AUTHKEY_FILE '$TS_AUTHKEY_FILE' does not exist"
  fi
  [[ -z "$SECRETS_FROM" || -f "$SECRETS_FROM" ]] || die "SECRETS_FROM '$SECRETS_FROM' does not exist"
}

# --------------------------------------------------------------- preflight

preflight() {
  log "Preflight"
  [[ $EUID -eq 0 || $DRY_RUN -eq 1 ]] || die "run as root (sudo); dry runs may be unprivileged"
  [[ "$(uname -s)" == Linux ]] || die "Linux only"
  if ! command -v systemctl >/dev/null; then
    [[ $DRY_RUN -eq 1 ]] && note "WARNING: systemd not found (tolerated in --dry-run only)" || die "systemd is required"
  fi

  case "$(uname -m)" in
    x86_64) ARCH=amd64 ;;
    aarch64|arm64) ARCH=arm64; note "arm64: newer and less tested than amd64 (README)" ;;
    *) die "unsupported architecture $(uname -m)" ;;
  esac

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
  log "Firewall (ufw): default deny inbound"
  run ufw default deny incoming
  run ufw default allow outgoing
  if [[ "$PERIMETER" == public-https ]]; then
    run ufw limit "$SSH_PORT"/tcp comment 'ssh rate-limited'
    run ufw allow 80/tcp comment 'acme http-01'
    run ufw allow 443/tcp comment 'ikenga https'
  else
    # Tailnet: nothing on the public interface at all. SSH only over tailscale0.
    run ufw allow in on tailscale0 to any port "$SSH_PORT" proto tcp comment 'ssh over tailnet'
    run ufw allow in on tailscale0 to any port 4000 proto tcp comment 'ikenga over tailnet'
    note "SSH on the public interface is CLOSED. Confirm tailnet SSH works from a second session before you leave this one."
  fi
  run ufw --force enable
  changed "ufw enabled ($PERIMETER rules)"
}

# --------------------------------------------------------------- perimeter

perimeter_tailnet() {
  log "Perimeter: tailnet"
  if ! command -v tailscale >/dev/null; then
    run sh -c 'curl -fsSL https://tailscale.com/install.sh | sh'
    changed "tailscale installed"
  fi
  if ! tailscale status >/dev/null 2>&1; then
    # Auth key read from a file, passed via env so it never appears in argv or `ps`.
    run sh -c "TS_AUTHKEY=\"\$(cat '$TS_AUTHKEY_FILE')\" tailscale up --auth-key=\"\$TS_AUTHKEY\" ${TS_HOSTNAME:+--hostname='$TS_HOSTNAME'} --ssh=false"
    changed "joined tailnet"
  fi
  if [[ $DRY_RUN -eq 0 ]]; then
    TS_IP="$(tailscale ip -4 | head -1)"
    [[ -n "$TS_IP" ]] || die "no tailnet address after 'tailscale up'"
  else
    TS_IP="<tailnet-ip>"
  fi
  IKENGA_HOST_VALUE="$TS_IP"
  note "Operator action: add an ACL rule letting your users reach this host on tcp:4000 (default-deny tailnet)."
}

perimeter_public() {
  log "Perimeter: public-https (Caddy + Let's Encrypt)"
  if ! command -v caddy >/dev/null; then
    run sh -c 'curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/gpg.key | gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg'
    run sh -c "curl -fsSL https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt -o /etc/apt/sources.list.d/caddy-stable.list"
    run env DEBIAN_FRONTEND=noninteractive apt-get update -y
    apt_install caddy
  fi
  # Request body cap and security headers. The daemon has no trusted-proxy
  # setting yet (WP-P4): it sees 127.0.0.1 for every client, so per-IP rate
  # limits belong here, not in the daemon, until that lands.
  local conf="$PUBLIC_HOST {
	encode zstd gzip
	request_body { max_size 12MB }
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
      *) die "unknown AGENT_CLI '$cli' (allowed: claude)" ;;
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

write_env() {
  log "Credentials and environment ($ENV_FILE)"
  if [[ $DRY_RUN -eq 1 ]]; then
    note "[dry-run] ensure $ENV_FILE (root:root 600), IKENGA_HOST, IKENGA_PUBLIC_URL, vault key${TIER:+, token if t0}"
    return
  fi
  install -d -m 0755 "$INSTALL_DIR"
  [[ -f "$ENV_FILE" ]] && cp -a "$ENV_FILE" "$ENV_FILE.bak-$(date +%Y%m%d-%H%M%S)"
  touch "$ENV_FILE"; chmod 600 "$ENV_FILE"; chown root:root "$ENV_FILE"

  ensure_var IKENGA_HOST "$IKENGA_HOST_VALUE"
  [[ -n "${IKENGA_PUBLIC_URL_VALUE:-}" ]] && ensure_var IKENGA_PUBLIC_URL "$IKENGA_PUBLIC_URL_VALUE"
  ensure_var IKENGA_VAULT_KEY "$(openssl rand -hex 32)"
  if [[ "$TIER" == t0 ]]; then
    ensure_var IKENGA_AUTH_TOKEN "$(openssl rand -hex 32)"
  elif [[ "$PERIMETER" == tailnet ]]; then
    # Plain HTTP on a tailnet: the session cookie cannot be Secure (README).
    ensure_var IKENGA_INSECURE_COOKIE true
  fi

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
  systemctl enable --now "$svc"
  changed "service $svc enabled and started"
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

# ------------------------------------------------------------------ main

validate_profile
IKENGA_HOST_VALUE="127.0.0.1"; IKENGA_PUBLIC_URL_VALUE=""; ARCH=""; TS_IP=""
preflight
confirm
harden_base
if [[ "$PERIMETER" == tailnet ]]; then perimeter_tailnet; else perimeter_public; fi
install_deps
install_daemon
write_env
install_service
firewall
verify
summary
