#!/usr/bin/env node
// Materialize the engine-claude-code builtin's dist/ directory before Tauri
// bundles it. The source tree stores a git-tracked *symlink* at
//   src-tauri/resources/builtin-pkgs/com.ikenga.engine-claude-code/dist
// that points at the ikenga-pkgs workspace output. On Windows (and on CI
// without core.symlinks=true) the symlink is checked out as a regular text
// file. Tauri then packages that file as `dist`, and the NSIS installer fails
// with "Error opening file for writing: ...dist\acp-engine.d.ts" because the
// installer cannot create files under a file named `dist`.
//
// This script replaces the symlink/file with a real copy of the built dist.
// It prefers the live workspace source, falls back to the npm-installed
// package, and fails gracefully if neither exists.

import { existsSync, lstatSync, mkdirSync, rmSync, cpSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const SHELL_ROOT = resolve(__filename, '..', '..');
const DEST = resolve(
  SHELL_ROOT,
  'src-tauri/resources/builtin-pkgs/com.ikenga.engine-claude-code/dist'
);

const CANDIDATES = [
  // Freshly-built source in the sibling workspace (preferred in CI / dev).
  resolve(SHELL_ROOT, '../ikenga-pkgs/packages/engine/claude-code/dist'),
  // Installed npm package fallback for local builds that don't rebuild ikenga-pkgs.
  resolve(SHELL_ROOT, 'node_modules/@ikenga/pkg-engine-claude-code/dist'),
];

function findSource() {
  for (const p of CANDIDATES) {
    if (existsSync(p) && lstatSync(p).isDirectory()) {
      return p;
    }
  }
  return null;
}

function main() {
  const src = findSource();

  if (!src) {
    // No built dist available. If a stale symlink/file is present, delete it
    // so the Tauri bundle never ships a bogus file. A missing dist is safe for
    // the runtime (the builtin engine is manifest-only).
    if (existsSync(DEST) && !lstatSync(DEST).isDirectory()) {
      console.warn(
        `[materialize-engine-cc] no source found; removing bogus dist file at ${DEST}`
      );
      rmSync(DEST, { force: true });
    } else {
      console.warn(
        '[materialize-engine-cc] no source found; run `pnpm -F @ikenga/pkg-engine-claude-code build` in ikenga-pkgs or `bun install` in shell.'
      );
    }
    return;
  }

  if (existsSync(DEST)) {
    rmSync(DEST, { recursive: true, force: true });
  }
  mkdirSync(dirname(DEST), { recursive: true });
  cpSync(src, DEST, { recursive: true, dereference: true });
  console.log(`[materialize-engine-cc] ${src} -> ${DEST}`);
}

main();
