// Post-restart "Updated to X" toast state (`designs/system-flows.html`
// `update-flow` step 4 / `paintUpdate`'s final `upRestart()` toast: "Updated
// to 0.9.1 · 2 sessions resumed").
//
// `restartApp()` (`src/lib/updater/updater.ts`) tears the whole process down
// and relaunches it — nothing in memory (Zustand stores included) survives
// that. So the one thing worth showing after the relaunch has to be written
// somewhere that does survive it: `localStorage`, the same mechanism
// `snooze.ts` already uses for the updater's other cross-restart state.
//
// Written right before the click that calls `restart()`; read once on the
// next boot by `<PostRestartUpdateToast>`. If the app that comes back up
// isn't actually on the version we were expecting (the relaunch landed on a
// different build, or the marker is stale from a session that never
// restarted), the marker is dropped silently rather than shown — a toast
// claiming an update that didn't happen would be worse than no toast.

import { getAppVersion } from '@/lib/transport';
import { scopedPersistName } from '@/lib/window/window-context';

const STORAGE_KEY = 'ikenga.updater.pending-restart';

export interface PendingRestart {
	version: string;
	notes: string;
	/** Live (persistent) session count captured just before the restart. */
	sessionsBefore: number;
	at: number;
}

function storageKey(): string {
	return scopedPersistName(STORAGE_KEY);
}

/** Called right before `restart()` relaunches the app. */
export function markPendingRestart(info: Omit<PendingRestart, 'at'>): void {
	try {
		localStorage.setItem(storageKey(), JSON.stringify({ ...info, at: Date.now() }));
	} catch {
		// localStorage unavailable (private window, quota) — the toast is a
		// nicety, not load-bearing; the restart itself doesn't depend on it.
	}
}

/** Drops any pending marker without acting on it — used once consumed, and
 *  by the "not actually the version we expected" bail-out below. */
function clearPendingRestart(): void {
	try {
		localStorage.removeItem(storageKey());
	} catch {
		// Same as above — best effort.
	}
}

/**
 * Reads and clears the marker (always clears — a marker is consumed at most
 * once, win or lose). Returns it only when the running app's version matches
 * what was marked; otherwise returns null.
 */
export async function consumePendingRestartIfMatching(): Promise<PendingRestart | null> {
	let raw: string | null = null;
	try {
		raw = localStorage.getItem(storageKey());
	} catch {
		return null;
	}
	if (!raw) return null;

	let parsed: PendingRestart;
	try {
		parsed = JSON.parse(raw) as PendingRestart;
	} catch {
		clearPendingRestart();
		return null;
	}
	clearPendingRestart();

	if (!parsed.version) return null;
	const current = await getAppVersion();
	if (current !== parsed.version) return null;
	return parsed;
}
