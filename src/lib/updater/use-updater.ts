// React hook that owns the updater check/install state. Polls the endpoint
// on app boot + every 6h, and exposes:
//   - available: UpdateInfo | null
//   - installing: boolean, bytesDownloaded / totalBytes for progress
//   - install(): kicks off downloadAndInstall; never relaunches
//   - restart(): the only path that relaunches, always user-driven
//   - check(): manual re-check (e.g. "Check now" on the About page)
//   - lastCheckedAt: epoch ms of the last successful check
//   - checking: true while a check is in flight
//
// Hook is used at multiple call sites (banner + About page + the WP-41
// update sheet's Shell tab + the status bar's progress segment). WP-41 made
// the state a shared store (`updater-store.ts`) rather than per-instance
// `useState`: a download the banner starts has to show up as "downloading"
// in the sheet the same click opens, and in the status bar — three separate
// component trees that all need to agree on one live download. The 6h
// auto-check still fires from the banner instance mounted in workspace.tsx;
// pass `{ autoPoll: false }` from secondary call sites so only the banner
// owns the timer (now a module-level guard in `updater-store.ts`, so this is
// belt-and-suspenders rather than load-bearing).

import { useEffect } from 'react';
import type { UpdateInfo } from '@/lib/updater/updater';
import { ensureUpdaterPolling, useUpdaterStore } from '@/lib/updater/updater-store';

export type UpdaterState = {
	available: UpdateInfo | null;
	installing: boolean;
	/** True once the bundle is installed and only a restart remains. The UI
	 *  should surface a "Restart to finish" action rather than relaunching
	 *  out from under the user (see updater.ts for why the two are split). */
	installed: boolean;
	bytesDownloaded: number;
	totalBytes: number | null;
	error: string | null;
	checking: boolean;
	lastCheckedAt: number | null;
	check: () => Promise<void>;
	/** Download + install. Never relaunches — every path (manual click and the
	 *  opt-in background auto-install alike) holds at the `installed` state so
	 *  the restart stays a user-driven act that can't discard in-flight work. */
	install: () => Promise<void>;
	/** Relaunch to complete an installed update. */
	restart: () => Promise<void>;
};

export interface UseUpdaterOptions {
	/** Default true. Pass false to skip the 6h interval timer (e.g. secondary
	 *  call sites where another instance already owns the polling). */
	autoPoll?: boolean;
	/** Default true. Pass false to suppress automatic checks entirely — no
	 *  boot check and no interval. The manual `check()` still works (the About
	 *  page's "Check now" button). Driven by the `updates.autoCheck` setting. */
	enabled?: boolean;
}

export function useUpdater(options?: UseUpdaterOptions): UpdaterState {
	const autoPoll = options?.autoPoll ?? true;
	const enabled = options?.enabled ?? true;

	// Subscribes to the whole store — this hook has a handful of call sites,
	// not a hot list render, so the simplicity of "re-render on any updater
	// state change" outweighs the value of field-level selectors here.
	const state = useUpdaterStore();

	// biome-ignore lint/correctness/useExhaustiveDependencies: deliberately only
	// on mount / when `enabled` or `autoPoll` change — `state.check` is a stable
	// store action, and re-running this effect on every store update
	// (available/installing/…) would re-trigger the boot check in a loop.
	useEffect(() => {
		if (!enabled) return;
		void state.check();
		if (autoPoll) ensureUpdaterPolling();
	}, [enabled, autoPoll]);

	return {
		available: state.available,
		installing: state.installing,
		installed: state.installed,
		bytesDownloaded: state.bytesDownloaded,
		totalBytes: state.totalBytes,
		error: state.error,
		checking: state.checking,
		lastCheckedAt: state.lastCheckedAt,
		check: state.check,
		install: state.install,
		restart: state.restart,
	};
}
