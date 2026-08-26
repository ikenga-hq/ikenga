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
    echo "  ${key}: set in ${ENV_FILE}"
  fi
}

echo "--> Server secrets"

# 1. Vault key for Stronghold-equivalent secret storage.
#
#    RESERVED, NOT YET CONSUMED. `ikenga-server` does not read this variable
#    today — the headless vault is WP-12b (ikenga#100), and `secrets_set`
#    currently answers "not implemented in the headless daemon". It is
#    generated now so the value is stable from the first boot and the vault
#    lands on an existing key rather than minting one after data exists.
ensure_var IKENGA_VAULT_KEY "$(openssl rand -hex 32)"

# 2. Bearer token for the API + WebSocket surface.
#
#    The daemon fails closed: with no token it mints a random one at startup
#    and logs it. That is safe but useless for a managed deploy — the token
#    would change on every restart, so every client breaks after a reboot.
#    Pin a stable one here. `ikenga-server` reads IKENGA_AUTH_TOKEN directly
#    (clap `env = "IKENGA_AUTH_TOKEN"`), and the systemd unit loads this file.
ensure_var IKENGA_AUTH_TOKEN "$(openssl rand -hex 32)"

# 3. Bind address.
#
#    The systemd unit deliberately passes no --host, so an absent or malformed
#    env file leaves the daemon on loopback — unreachable, which is the safe
#    way to fail. Pin the tailnet address here so the daemon is reachable over
#    the tailnet and NOWHERE else. Binding 0.0.0.0 on a box with a public IP
#    publishes a shell to the internet with only the bearer token in front of
#    it; the perimeter is the bind, not the token.
# `|| true` is load-bearing: this script runs under `set -euo pipefail`, and
# with pipefail a failing `tailscale ip` (installed but not logged in) would
# fail the whole pipeline and abort the bootstrap partway through.
TS_IP=""
if command -v tailscale >/dev/null 2>&1; then
  TS_IP="$( { tailscale ip -4 2>/dev/null || true; } | head -1 )"
fi
if [ -n "$TS_IP" ]; then
  ensure_var IKENGA_HOST "$TS_IP"
else
  echo "  IKENGA_HOST: no Tailscale address found — leaving unset (daemon stays on 127.0.0.1)"
  echo "               Set it by hand once the host is on the tailnet."
fi

echo "--> Agent credentials"

# Agent API keys go into the SAME env file, not into hand-written config files.
#
# An earlier revision did `printf '{"apiKey":"%s"}' > ~/.claude.json`. That is
# wrong twice over:
#   1. It TRUNCATES ~/.claude.json, which is Claude Code's entire config —
#      project history, MCP servers, oauth account, onboarding state. On any
#      host where the CLI has been used, running bootstrap destroyed all of it,
#      irrecoverably, with no backup.
#   2. `apiKey` is not a key Claude Code reads. The supported mechanism is the
#      ANTHROPIC_API_KEY environment variable — so the write was destructive
#      AND non-functional.
#
# systemd loads this file via EnvironmentFile, and ikenga-server's engine
# adapters inherit their environment, so the CLI children see these directly.

if [ -n "${ANTHROPIC_API_KEY:-}" ]; then
  ensure_var ANTHROPIC_API_KEY "${ANTHROPIC_API_KEY}"
else
  echo "  ANTHROPIC_API_KEY: not set, skipping"
fi

if [ -n "${GEMINI_API_KEY:-}" ]; then
  ensure_var GEMINI_API_KEY "${GEMINI_API_KEY}"
else
  echo "  GEMINI_API_KEY: not set, skipping"
fi

echo
echo "Task 0 bootstrap complete."
echo "Read the auth token with:  sudo grep '^IKENGA_AUTH_TOKEN=' ${ENV_FILE}"
