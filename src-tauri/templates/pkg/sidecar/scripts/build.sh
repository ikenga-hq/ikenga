#!/usr/bin/env bash
# Cross-compile the sidecar binary for every declared target triple.
# Wire your actual build steps here (bun build --compile, cargo build
# --target=…, etc.). The manifest's `bin: "bin/{target}/{{slug}}"` is
# expanded at load time by the kernel.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

mkdir -p bin

for TARGET in x86_64-unknown-linux-gnu aarch64-apple-darwin x86_64-pc-windows-msvc; do
  echo "[build] target=$TARGET"
  mkdir -p "bin/$TARGET"
  # Replace this with your actual compile step:
  # bun build --compile --target=bun-${TARGET//-/_} src/index.ts --outfile "bin/$TARGET/{{slug}}"
  echo "TODO: compile bin/$TARGET/{{slug}}" > "bin/$TARGET/{{slug}}"
done

echo "[build] done"
