#!/bin/bash
set -euo pipefail

# Task 0 (ikenga#112 / WP-14a / G-30): seed the server's own secrets and plant agent
# credentials on a fresh host.
#
# Secrets are never echoed. Printing a generated key puts it in the terminal
# scrollback, in journald if this runs under systemd, and in CI logs if it ever
# runs there — all places it outlives the operator's attention.

ENV_FILE="${IKENGA_ENV_FILE:-/opt/ikenga/.env}"

echo "=================================================="
echo "Ikenga Remote Server: Task 0 Credential Bootstrap"
echo "=================================================="

mkdir -p "$(dirname "$ENV_FILE")"
touch "$ENV_FILE"
chmod 600 "$ENV_FILE"

# Append KEY=VALUE only when KEY is not already present, so re-running this is
# idempotent and never clobbers a credential nobody has another copy of.
ensure_var() {
  local key="$1" value="$2"
  if grep -qE "^${key}=" "$ENV_FILE"; then
    echo "  ${key}: already set, leaving as-is"
  else
    printf '%s=%s\n' "$key" "$value" >> "$ENV_FILE"
    echo "  ${key}: generated and written to ${ENV_FILE}"
  fi
}

echo "--> Server secrets"

# 1. Vault key for Stronghold-equivalent secret storage.
ensure_var IKENGA_VAULT_KEY "$(openssl rand -hex 32)"

# 2. Bearer token for the API + WebSocket surface.
#
#    The daemon fails closed: with no token it mints a random one at startup
#    and logs it. That is safe but useless for a managed deploy — the token
#    would change on every restart, so every client breaks after a reboot.
#    Pin a stable one here. `ikenga-server` reads IKENGA_AUTH_TOKEN directly
#    (clap `env = "IKENGA_AUTH_TOKEN"`), and the systemd unit loads this file.
ensure_var IKENGA_AUTH_TOKEN "$(openssl rand -hex 32)"

echo "--> Agent credentials"

# Planted files hold live API keys, so they are created with a private umask
# rather than whatever the caller happened to have set.
umask 077

if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
  mkdir -p ~/.claude
  printf '{"apiKey": "%s"}\n' "${ANTHROPIC_API_KEY}" > ~/.claude.json
  chmod 600 ~/.claude.json
  echo "  ANTHROPIC_API_KEY: planted into ~/.claude.json (mode 600)"
else
  echo "  ANTHROPIC_API_KEY: not set, skipping"
fi

if [ -n "${GEMINI_API_KEY:-}" ]; then
  mkdir -p ~/.gemini/antigravity
  printf '{"apiKey": "%s"}\n' "${GEMINI_API_KEY}" > ~/.gemini/antigravity/env.json
  chmod 600 ~/.gemini/antigravity/env.json
  echo "  GEMINI_API_KEY: planted into ~/.gemini/antigravity/env.json (mode 600)"
else
  echo "  GEMINI_API_KEY: not set, skipping"
fi

echo
echo "Task 0 bootstrap complete."
echo "Read the auth token with:  sudo grep '^IKENGA_AUTH_TOKEN=' ${ENV_FILE}"
