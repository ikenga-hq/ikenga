// App lock — the per-window mirror of Rust's lock state (WP-72).
//
// Rust (`commands/app_lock.rs`) owns the lock. This store only mirrors it, so
// every window (main and `detached-*`) renders the same overlay, and a reload
// shows the lock again straight away. `startAppLockSync()` runs once per
// window. It keeps the mirror fresh and reports user activity for the idle
// clock:
//   - on `app-lock://changed`, refetch;
//   - on focus and visibility, refetch, because `app_lock_status` also runs
//     the idle check, so a laptop waking from sleep locks at once;
//   - on pointer, key or wheel input, call `app_lock_touch`, at most once per
//     `ACTIVITY_THROTTLE_MS`, and never while locked.

import { create } from 'zustand';

import { type AppLockStatus, appLockStatus, appLockTouch, onAppLockChanged } from '@/lib/tauri-cmd';

import { shouldReportActivity } from './app-lock-model';

interface AppLockStore {
	/** `null` until loaded, in a remote web session, or when the command is
	 *  unreachable (Vite-only dev). All three mean "no lock here". */
	status: AppLockStatus | null;
	setStatus: (status: AppLockStatus | null) => void;
	refresh: () => Promise<AppLockStatus | null>;
}

export const useAppLockStore = create<AppLockStore>((set) => ({
	status: null,
	setStatus: (status) => set({ status }),
	refresh: async () => {
		try {
			const status = await appLockStatus();
			set({ status });
			return status;
		} catch {
			// Not a Tauri window, or the command isn't registered in this
			// build. Keep whatever we had rather than dropping a live lock.
			return useAppLockStore.getState().status;
		}
	},
}));

/** True while this window should show the lock. */
export function useAppLocked(): boolean {
	return useAppLockStore((s) => s.status?.locked === true);
}

const ACTIVITY_EVENTS = ['pointerdown', 'keydown', 'wheel', 'touchstart'] as const;

let refs = 0;
let teardown: (() => void) | null = null;

/** Start the mirror for this window. Refcounted, so StrictMode's double
 *  mount and a second caller are harmless. Returns the release. */
export function startAppLockSync(): () => void {
	refs += 1;
	if (refs === 1) teardown = install();
	let released = false;
	return () => {
		if (released) return;
		released = true;
		refs -= 1;
		if (refs === 0) {
			teardown?.();
			teardown = null;
		}
	};
}

function install(): () => void {
	if (typeof window === 'undefined') return () => {};
	const { refresh } = useAppLockStore.getState();
	void refresh();

	let disposed = false;
	let unlisten: (() => void) | null = null;
	onAppLockChanged(() => {
		void useAppLockStore.getState().refresh();
	})
		.then((fn) => {
			if (disposed) fn();
			else unlisten = fn;
		})
		.catch(() => {});

	let lastSent: number | null = null;
	const onActivity = () => {
		const status = useAppLockStore.getState().status;
		if (!status || status.locked || !status.idleEnabled) return;
		const now = Date.now();
		if (!shouldReportActivity(lastSent, now)) return;
		lastSent = now;
		appLockTouch().catch(() => {});
	};
	const onWake = () => {
		if (document.visibilityState === 'hidden') return;
		void useAppLockStore.getState().refresh();
	};

	for (const type of ACTIVITY_EVENTS) {
		window.addEventListener(type, onActivity, { capture: true, passive: true });
	}
	window.addEventListener('focus', onWake);
	document.addEventListener('visibilitychange', onWake);

	return () => {
		disposed = true;
		unlisten?.();
		for (const type of ACTIVITY_EVENTS) {
			window.removeEventListener(type, onActivity, { capture: true });
		}
		window.removeEventListener('focus', onWake);
		document.removeEventListener('visibilitychange', onWake);
	};
}

/** Test seam: drop the refcount and listeners between tests. */
export function resetAppLockSyncForTests(): void {
	teardown?.();
	teardown = null;
	refs = 0;
	useAppLockStore.setState({ status: null });
}
