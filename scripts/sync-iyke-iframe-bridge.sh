#!/usr/bin/env bash
# Sync the canonical iframe bridge from ikenga-desktop to each
# sidecar's src dir. Run after editing
# `src/lib/iyke/iframe-bridge.ts`.
#
# Each sidecar then imports `./iyke-bridge` and calls
# `mountIykeIframeBridge()` once at app startup.

set -euo pipefail

SRC="$(cd "$(dirname "$0")"/.. && pwd)/src/lib/iyke/iframe-bridge.ts"
ROOT="$(cd "$(dirname "$0")"/../.. && pwd)"

if [[ ! -f "$SRC" ]]; then
  echo "error: bridge source not found at $SRC" >&2
  exit 1
fi

# All three former destinations lived inside royalti-video-engine, which was
# retired on 2026-09-08 (WP-16) and deleted:
#
#   $ROOT/royalti-video-engine/storyboard-app/src/iyke-bridge.ts
#   $ROOT/royalti-video-engine/src/iyke-bridge.ts
#   $ROOT/royalti-video-engine/hyperframes-projects/*/preview/iyke-bridge.ts
#
# Leaving them was worse than a broken path. The copy loop ran `mkdir -p`
# before `cp`, so this script would silently RE-CREATE royalti-video-engine/
# as ghost directories holding one file each, on a tree where that directory
# is meant to be gone -- and report success while doing it.
#
# Nothing outside the engine consumed this bridge, so the list is empty rather
# than repointed; inventing a destination would be guessing. Add real entries
# here when a sidecar needs the bridge. The re-bundle step below is
# independent and still does useful work.
DESTS=()

if [[ ${#DESTS[@]} -eq 0 ]]; then
  echo "no sync destinations configured (see the note above) - skipping copy"
else
  for d in "${DESTS[@]}"; do
    [[ -z "$d" ]] && continue
    mkdir -p "$(dirname "$d")"
    cp "$SRC" "$d"
    echo "  -> $d"
  done
  echo "synced ${#DESTS[@]} copies"
fi

# Re-bundle the standalone IIFE used by the viewer-server's HTML injection.
# Without this, design previews opened via HtmlFrame would still see the
# previous bridge version.
PA_DESKTOP_ROOT="$(cd "$(dirname "$0")"/.. && pwd)"
if command -v bun >/dev/null 2>&1; then
  ( cd "$PA_DESKTOP_ROOT" && bun run iyke:bundle >/dev/null )
  echo "  → $PA_DESKTOP_ROOT/src-tauri/resources/iyke-iframe-bridge.js (re-bundled)"
else
  echo "warn: bun not found — skipping viewer-server bridge re-bundle" >&2
fi
