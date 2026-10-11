#!/bin/bash
# run-sender.sh — wrapper runner for the separate outbox send worker (D-B13).
set -euo pipefail

export PATH="/usr/local/bin:/usr/bin:/bin:$PATH"
export NODE_ENV=production

# systemd LoadCredential provides %d/env, env-vault honours PA_ACTIONS_ENV_FILE
if [[ -n "${CREDENTIALS_DIRECTORY:-}" && -f "$CREDENTIALS_DIRECTORY/env" ]]; then
  export PA_ACTIONS_ENV_FILE="$CREDENTIALS_DIRECTORY/env"
fi

ENTRY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if [[ -f "$ENTRY_DIR/dist/lib/send-worker.js" ]]; then
  exec node "$ENTRY_DIR/dist/lib/send-worker.js" "$@"
elif [[ -f "$ENTRY_DIR/lib/send-worker.js" ]]; then
  exec node "$ENTRY_DIR/lib/send-worker.js" "$@"
else
  echo "[run-sender] send-worker.js entrypoint not found in $ENTRY_DIR" >&2
  exit 1
fi
