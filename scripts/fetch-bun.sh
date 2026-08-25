#!/usr/bin/env bash
# Download a pinned Bun release for a single target, verify SHA-256 against a
# baked-in expected value, and drop the binary at
# `src-tauri/resources/bun/<target>/bun` (or `bun.exe` on Windows targets).
#
# Pin policy (per ADR-010 + 2026-05-13 decision): pin BUN_VERSION + per-target
# SHA-256s here. Bun bumps land via a deliberate PR that updates both. Do NOT
# auto-resolve the latest release.
#
# Usage:
#   bash scripts/fetch-bun.sh --target <bun-target>
#   bash scripts/fetch-bun.sh --target linux-x64 --out custom/dir
#
# Targets (Bun's own naming; differs from Rust triples):
#   linux-x64, linux-aarch64, darwin-x64, darwin-aarch64, windows-x64
#
# Idempotent: if the binary already exists at the destination AND its sha256
# matches the pinned value, nothing happens. Re-run after bumping the pin to
# replace the cached copy.

set -euo pipefail

# ─── Pinned release ─────────────────────────────────────────────────────────
BUN_VERSION="1.4.0"

# SHASUMS256.txt from https://github.com/oven-sh/bun/releases/tag/bun-v1.4.0
# Case statement (not `declare -A`) so this works on macOS's default
# /bin/bash 3.2 — associative arrays only landed in bash 4.
expected_sha_for() {
  case "$1" in
    linux-x64)       echo "2d03fb5fb83ac8b567aca0a281b2ce1a1a19d488f56c2968d88c3f25e92fe452" ;;
    linux-aarch64)   echo "4b1a332ee861983eb93bcfe6f770fff94e3e31b2c388bdaea3c8ed35e58eed0e" ;;
    darwin-x64)      echo "1d0211b8f1dc991182344687ad15e72ee86f154845a5f7fa477994cd341dd9b0" ;;
    darwin-aarch64)  echo "c669e97f6164e1c96e0701748db98dfa77492908cbd8394c7557134a735de381" ;;
    windows-x64)     echo "e6f093d39da486b20262ca8cdd5ed6a9e8bc9c2f275b78e6d3a0c5b28cc95901" ;;
    windows-aarch64) echo "f473bfe2df73ee770548c93dd5d380aea7120c218ec2aa1afdd0bbba7bf18c47" ;;
    *) return 1 ;;
  esac
}
KNOWN_TARGETS="linux-x64 linux-aarch64 darwin-x64 darwin-aarch64 windows-x64 windows-aarch64"

# ─── Args ───────────────────────────────────────────────────────────────────
TARGET=""
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SHELL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_ROOT="$SHELL_DIR/src-tauri/resources/bun"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target) TARGET="$2"; shift 2 ;;
    --out)    OUT_ROOT="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,30p' "$0"; exit 0 ;;
    *)
      echo "fetch-bun.sh: unknown arg: $1" >&2; exit 2 ;;
  esac
done

if [[ -z "$TARGET" ]]; then
  # Auto-detect host so `bun run build` (and devs) don't need to pass it.
  # CI cross-builds always pass --target explicitly.
  uname_s="$(uname -s)"
  uname_m="$(uname -m)"
  case "$uname_s" in
    Linux)   plat="linux" ;;
    Darwin)  plat="darwin" ;;
    MINGW*|MSYS*|CYGWIN*) plat="windows" ;;
    *) echo "fetch-bun.sh: cannot auto-detect target on uname=$uname_s; pass --target" >&2; exit 2 ;;
  esac
  case "$uname_m" in
    x86_64|amd64) arch="x64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *) echo "fetch-bun.sh: cannot auto-detect arch from uname=$uname_m; pass --target" >&2; exit 2 ;;
  esac
  TARGET="$plat-$arch"
  echo "fetch-bun.sh: auto-detected host target = $TARGET"
fi

if ! EXPECTED_SHA="$(expected_sha_for "$TARGET")"; then
  echo "fetch-bun.sh: no pinned sha for target '$TARGET'. Known targets: $KNOWN_TARGETS" >&2
  exit 2
fi

# Windows ships bun.exe; everything else ships a bare `bun` ELF/Mach-O.
BIN_NAME="bun"
case "$TARGET" in
  windows-*) BIN_NAME="bun.exe" ;;
esac

OUT_DIR="$OUT_ROOT/$TARGET"
OUT_BIN="$OUT_DIR/$BIN_NAME"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "fetch-bun.sh: need sha256sum or shasum on PATH" >&2
    exit 1
  fi
}

# ─── Idempotent fast-path ───────────────────────────────────────────────────
if [[ -f "$OUT_BIN" ]]; then
  CACHED_SHA="$(sha256_of "$OUT_BIN")"
  # The pinned sha is for the .zip; once unzipped, hash of the binary itself
  # differs. We track the zip-sha alongside the binary in a sidecar file so
  # we can detect drift without re-downloading.
  if [[ -f "$OUT_BIN.zip-sha256" ]] && [[ "$(cat "$OUT_BIN.zip-sha256")" == "$EXPECTED_SHA" ]]; then
    echo "fetch-bun.sh: $TARGET bun-v$BUN_VERSION already present at $OUT_BIN (zip-sha matches pin)"
    exit 0
  fi
  echo "fetch-bun.sh: cached $OUT_BIN exists but zip-sha sidecar missing/mismatched — re-fetching"
fi

# ─── Download + verify ──────────────────────────────────────────────────────
mkdir -p "$OUT_DIR"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

ZIP_NAME="bun-$TARGET.zip"
URL="https://github.com/oven-sh/bun/releases/download/bun-v$BUN_VERSION/$ZIP_NAME"
ZIP_PATH="$TMP_DIR/$ZIP_NAME"

echo "fetch-bun.sh: downloading $URL"
if ! curl --fail --location --show-error --silent --output "$ZIP_PATH" "$URL"; then
  echo "fetch-bun.sh: download failed for $URL" >&2
  exit 1
fi

ACTUAL_SHA="$(sha256_of "$ZIP_PATH")"
if [[ "$ACTUAL_SHA" != "$EXPECTED_SHA" ]]; then
  echo "fetch-bun.sh: SHA-256 mismatch for $ZIP_NAME" >&2
  echo "  expected: $EXPECTED_SHA" >&2
  echo "  actual:   $ACTUAL_SHA" >&2
  exit 1
fi

# ─── Unzip ──────────────────────────────────────────────────────────────────
if ! command -v unzip >/dev/null 2>&1; then
  echo "fetch-bun.sh: need 'unzip' on PATH" >&2
  exit 1
fi

UNZIP_DIR="$TMP_DIR/unzip"
mkdir -p "$UNZIP_DIR"
unzip -q "$ZIP_PATH" -d "$UNZIP_DIR"

# Bun zips always contain a single dir like `bun-linux-x64/` with the binary
# inside. Locate the binary by name rather than guessing the dir layout.
SRC_BIN="$(find "$UNZIP_DIR" -maxdepth 3 -type f -name "$BIN_NAME" | head -n1)"
if [[ -z "$SRC_BIN" ]]; then
  echo "fetch-bun.sh: no $BIN_NAME found inside $ZIP_NAME after unzip" >&2
  exit 1
fi

mv "$SRC_BIN" "$OUT_BIN"
chmod +x "$OUT_BIN"
printf '%s\n' "$EXPECTED_SHA" > "$OUT_BIN.zip-sha256"

echo "fetch-bun.sh: installed bun-v$BUN_VERSION ($TARGET) → $OUT_BIN"
