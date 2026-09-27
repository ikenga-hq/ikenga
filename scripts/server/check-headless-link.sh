#!/bin/bash
set -euo pipefail

# Headless link gate: fail if an ikenga-server binary links the desktop stack
# (GTK / WebKit / JavaScriptCore). Such a binary will not start on a server:
#   error while loading shared libraries: libgdk-3.so.0
#
# Usage: check-headless-link.sh <path/to/ikenga-server>
#
# Shared by scripts/server/deploy.sh (before staging a release build) and the
# `headless` job in .github/workflows/ci.yml (on every Rust change), so the two
# can never disagree about what "headless" means. Linux only (uses ldd).

BIN="${1:?usage: check-headless-link.sh <path/to/ikenga-server>}"

if [[ ! -f "$BIN" ]]; then
  echo "error: $BIN not found — build it first (cargo build -p ikenga-server)." >&2
  exit 1
fi

if ! command -v ldd >/dev/null 2>&1; then
  # e.g. deploy.sh run on a macOS host (which it already warns about).
  echo "warning: ldd not available on this host; headless link gate skipped." >&2
  exit 0
fi

# Capture first: a failing ldd (not a dynamic executable, wrong arch) must fail
# the gate, not pass it by producing no output for grep to match.
if ! DEPS="$(ldd "$BIN" 2>&1)"; then
  echo "error: ldd $BIN failed:" >&2
  echo "$DEPS" >&2
  exit 1
fi

if grep -iE "gtk|webkit|javascriptcore" <<<"$DEPS" >&2; then
  echo "error: ikenga-server links the desktop stack (above); it will not run headless." >&2
  exit 1
fi
echo "    ok — $(wc -l <<<"$DEPS") shared deps, no GTK/WebKit"
