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

CARGO_FLAGS=("--manifest-path" "$SHELL_DIR/src-tauri/Cargo.toml" "--bin" "ikenga-server")
if [[ "$PROFILE" == "release" ]]; then
  CARGO_FLAGS+=("--release")
fi

echo "==> Building ikenga-server ($PROFILE)"
cargo build "${CARGO_FLAGS[@]}"

echo "==> Building frontend SPA"
(cd "$SHELL_DIR" && bun run build)

echo "==> Staging artifacts into $OUT_DIR"
mkdir -p "$OUT_DIR/bin"
cp "$SHELL_DIR/src-tauri/target/$PROFILE/ikenga-server" "$OUT_DIR/bin/ikenga-server"
rm -rf "$OUT_DIR/dist"
cp -r "$SHELL_DIR/dist" "$OUT_DIR/dist"

echo "==> Done."
echo "    binary: $OUT_DIR/bin/ikenga-server"
echo "    assets: $OUT_DIR/dist"
echo
echo "Deploy with:  rsync -a $OUT_DIR/ <host>:/opt/ikenga/"
echo "Then on the host: shell/scripts/server/bootstrap-credentials.sh && systemctl restart ikenga-server"
