#!/bin/bash
set -euo pipefail

# Build the ikenga-server binary + the SPA it serves.
#
# This script lives in shell/scripts/server/deploy.sh and runs from a clean
# checkout of shell alone.

SHELL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT_DIR="${OUT_DIR:-$SHELL_DIR/scripts/server/out}"
PROFILE="${PROFILE:-release}"

if [[ ! -d "$SHELL_DIR/src-tauri" ]]; then
  echo "error: $SHELL_DIR/src-tauri not found." >&2
  exit 1
fi

# The staged binary is rsynced to a remote host, so the target matters. Without
# an explicit TARGET this builds for the machine running the script — deploying
# from a macOS or Windows laptop then ships a binary the Linux server cannot
# execute, and the only symptom is "Exec format error" at systemd start.
TARGET="${TARGET:-}"
if [[ -n "$TARGET" ]]; then
  CARGO_FLAGS=("--manifest-path" "$SHELL_DIR/src-tauri/Cargo.toml" "--bin" "ikenga-server" "--target" "$TARGET")
  BIN_DIR="$SHELL_DIR/src-tauri/target/$TARGET/$PROFILE"
else
  CARGO_FLAGS=("--manifest-path" "$SHELL_DIR/src-tauri/Cargo.toml" "--bin" "ikenga-server")
  BIN_DIR="$SHELL_DIR/src-tauri/target/$PROFILE"
  HOST_TRIPLE="$(rustc -vV | awk '/^host: /{print $2}')"
  if [[ "$HOST_TRIPLE" != *linux* ]]; then
    echo "warning: building for host ($HOST_TRIPLE), not Linux." >&2
    echo "         Set TARGET=x86_64-unknown-linux-gnu to cross-compile for the server." >&2
  fi
fi

if [[ "$PROFILE" == "release" ]]; then
  CARGO_FLAGS+=("--release")
fi

echo "==> Building ikenga-server ($PROFILE${TARGET:+ · $TARGET})"
cargo build "${CARGO_FLAGS[@]}"

echo "==> Building frontend SPA"
(cd "$SHELL_DIR" && bun run build)

echo "==> Staging artifacts into $OUT_DIR"
mkdir -p "$OUT_DIR/bin"
cp "$BIN_DIR/ikenga-server" "$OUT_DIR/bin/ikenga-server"
rm -rf "$OUT_DIR/dist"
cp -r "$SHELL_DIR/dist" "$OUT_DIR/dist"

echo "==> Done."
echo "    binary: $OUT_DIR/bin/ikenga-server"
echo "    assets: $OUT_DIR/dist"
echo
echo "Deploy with:  rsync -a $OUT_DIR/ <host>:/opt/ikenga/"
echo "Then on the host: shell/scripts/server/bootstrap-credentials.sh && systemctl restart ikenga-server"
