// Sync fallback for "what cwd should I use when nothing else is configured?"
//
// Resolution order:
//   1. The first entry of the user's `fileRoots` allowlist.
//   2. The OS home directory (resolved once at boot via Tauri's
//      `path.homeDir()` — async, so we cache the result in a module
//      variable and expose a sync getter).
//   3. The literal `~` as a last resort for non-Tauri/test environments.
//
// Replaces the per-call-site hardcoded developer-machine fallbacks that
// used to live in dock.tsx, new-tab-menu.tsx, single-terminal.tsx, the
// engine adapters, and the sessions dialog.

import { homeDir } from '@/lib/transport/path-shim';

import { useShellStore } from './shell-store';

let cachedHome: string | null = null;

/** Resolve `$HOME` once at boot and cache it. Call from `main.tsx`. */
export async function initDefaultCwd(): Promise<void> {
	try {
		cachedHome = await homeDir();
	} catch {
		// Tauri path API unavailable (test env / web preview) — keep null
		// and let `defaultCwd()` fall through to `~`.
	}
}

/** Synchronous best-guess cwd for terminal/session fallbacks. */
export function defaultCwd(): string {
	const roots = useShellStore.getState().fileRoots;
	if (roots[0]) return roots[0];
	if (cachedHome) return cachedHome;
	return '~';
}
