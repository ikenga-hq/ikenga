// Lazy home-dir resolution via Tauri's path API. Cached after the first
// successful resolve so subsequent reads don't pay the IPC cost.
//
// Two access patterns:
//   - `loadHome()` — async, awaits the IPC. Use in async paths that can
//     wait for the real value (e.g. building filesystem paths).
//   - `getHomeSync()` / `shortPath()` — synchronous. Returns the cached
//     value if available, else falls back to graceful no-op behavior.
//     Use in render paths.
//
// `void loadHome()` is invoked at module-load so the cache is warm by the
// time the first render needs it.

import { homeDir } from '@/lib/transport/path-shim';

let homeCached = '';
let homePromise: Promise<string> | null = null;

export function loadHome(): Promise<string> {
	if (!homePromise) {
		homePromise = homeDir()
			.then((h) => {
				homeCached = h.replace(/\/$/, '');
				return homeCached;
			})
			.catch(() => homeCached);
	}
	return homePromise;
}

export function getHomeSync(): string {
	return homeCached;
}

// Replace a leading `$HOME` with `~`. Returns the input unchanged if the
// home dir hasn't resolved yet (first paint) or doesn't prefix the path.
export function shortPath(p: string): string {
	if (!p) return '—';
	const home = homeCached;
	if (home && p.startsWith(home)) return `~${p.slice(home.length)}`;
	return p;
}

// Kick off resolution eagerly.
void loadHome();
