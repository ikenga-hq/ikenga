// Shared store backing `useUpdater()`. Extracted for WP-41: the shell binary
// update now has THREE simultaneous surfaces reading/driving the same live
// state — <UpdaterBanner>, the status bar's progress segment, and the
// release-notes/downloading/restart steps in <UpdateSheet> — and they need to
// agree on it. A download started by clicking "Update now" in the banner has
// to show up as "downloading" when the sheet it just opened mounts its own
// `useUpdater()` call a tick later; per-hook-instance `useState` (the
// pre-WP-41 shape) can't do that since each instance only sees the state of
// whichever component happened to call `install()`.
//
// State transitions are unchanged from the pre-WP-41 hook (see
// `use-updater.test.ts`, which pins the install/restart split at this
// boundary and continues to pass unmodified): `check()` overwrites
// `available` + `lastCheckedAt`; `install()` resets `installing`/`installed`/
// `error` at entry (so a fresh call is never contaminated by a stale run —
// including across independent test cases sharing this module) and never
// calls `restartApp`; only `restart()` does.

import { create } from 'zustand';
import { checkForUpdate, installUpdate, restartApp, type UpdateInfo } from './updater';

export interface UpdaterStoreState {
	available: UpdateInfo | null;
	installing: boolean;
	installed: boolean;
	bytesDownloaded: number;
	totalBytes: number | null;
	error: string | null;
	checking: boolean;
	lastCheckedAt: number | null;
	check: () => Promise<void>;
	install: () => Promise<void>;
	restart: () => Promise<void>;
}

export const useUpdaterStore = create<UpdaterStoreState>((set, get) => ({
	available: null,
	installing: false,
	installed: false,
	bytesDownloaded: 0,
	totalBytes: null,
	error: null,
	checking: false,
	lastCheckedAt: null,

	check: async () => {
		set({ checking: true });
		try {
			const info = await checkForUpdate();
			set({ available: info, lastCheckedAt: Date.now() });
		} finally {
			set({ checking: false });
		}
	},

	install: async () => {
		const { available } = get();
		if (!available) return;
		set({ installing: true, installed: false, error: null });
		try {
			await installUpdate(available, (b, t) => {
				set({ bytesDownloaded: b, totalBytes: t });
			});
			// Install done — hold here, always. The relaunch is a separate,
			// deliberate step (see updater.ts): it protects unsaved work in
			// terminals and pkg panes that a surprise restart would throw away.
			set({ installing: false, installed: true });
		} catch (e) {
			set({ error: e instanceof Error ? e.message : String(e), installing: false });
		}
	},

	restart: async () => {
		try {
			await restartApp();
		} catch (e) {
			set({ error: e instanceof Error ? e.message : String(e) });
		}
	},
}));

/** Guards the 6h poll so it starts at most once regardless of how many
 *  `useUpdater({ autoPoll: true })` instances mount — in practice only
 *  <UpdaterBanner> ever passes `autoPoll: true`, but the guard makes that a
 *  convention, not a landmine. */
let pollHandle: number | null = null;
const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000; // 6h

export function ensureUpdaterPolling(): void {
	if (pollHandle !== null) return;
	pollHandle = window.setInterval(() => void useUpdaterStore.getState().check(), CHECK_INTERVAL_MS);
}

/** Test-only: drop the interval handle without clearing it (vitest fake
 *  timers already tear the timer down on module reset). Exists so a test
 *  that exercises polling twice doesn't trip the "already started" guard. */
export function _resetUpdaterPollingForTests(): void {
	if (pollHandle !== null) {
		window.clearInterval(pollHandle);
		pollHandle = null;
	}
}

/** Percentage 0–100, or null when the total size isn't known yet
 *  (`Content-Length` missing — show an indeterminate state, never invent a
 *  number). Shared by the banner, the status bar slot, and the sheet's
 *  downloading step so the three surfaces never round differently. */
export function progressPct(bytesDownloaded: number, totalBytes: number | null): number | null {
	if (!totalBytes || totalBytes <= 0) return null;
	return Math.min(100, Math.round((bytesDownloaded / totalBytes) * 100));
}
