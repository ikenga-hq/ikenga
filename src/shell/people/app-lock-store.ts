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
//     `ACTIVITY_THROTTLE_MS` plus one trailing send at the end of the window
//     if input came after the last send, and never while locked;
//   - input inside a pkg iframe counts too. The iframes are same-origin
//     (`sandbox="allow-scripts allow-same-origin"` + srcDoc), so when focus
//     moves into one the same listeners are attached to its window.
//
// Known gap: native pkg webviews (`pkg-webview-host.tsx`) are separate OS
// webviews, so input inside them never reaches this window. The idle-lock
// row in Profile › App lock says so.

import { create } from 'zustand';

import { type AppLockStatus, appLockStatus, appLockTouch, onAppLockChanged } from '@/lib/tauri-cmd';

import { shouldReportActivity, trailingDelay } from './app-lock-model';

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
	// Input arrived after `lastSent`, inside the current throttle window.
	let pending = false;
	let trailing: ReturnType<typeof setTimeout> | null = null;
	const reporting = () => {
		const status = useAppLockStore.getState().status;
		return Boolean(status && !status.locked && status.idleEnabled);
	};
	const send = (now: number) => {
		lastSent = now;
		pending = false;
		appLockTouch().catch(() => {});
	};
	const flushTrailing = () => {
		trailing = null;
		if (pending && reporting()) send(Date.now());
		pending = false;
	};
	const onActivity = () => {
		if (disposed || !reporting()) return;
		const now = Date.now();
		if (shouldReportActivity(lastSent, now)) {
			send(now);
			return;
		}
		pending = true;
		if (trailing === null && lastSent !== null) {
			trailing = setTimeout(flushTrailing, trailingDelay(lastSent, now));
		}
	};
	const onWake = () => {
		if (document.visibilityState === 'hidden') return;
		void useAppLockStore.getState().refresh();
	};

	const listenOn = (target: Window) => {
		for (const type of ACTIVITY_EVENTS) {
			target.addEventListener(type, onActivity, { capture: true, passive: true });
		}
	};
	const unlistenOn = (target: Window) => {
		for (const type of ACTIVITY_EVENTS) {
			target.removeEventListener(type, onActivity, { capture: true });
		}
	};

	// Same-origin pkg iframes: focus moving into one blurs this window with the
	// iframe as `activeElement`. That move is itself input (a click or Tab), and
	// from then on the iframe's own window reports through the same listeners.
	// A new srcDoc means a new window; the next focus-in attaches to it. Held
	// weakly so a removed frame's window can be collected; its listeners go with
	// it, and after teardown `onActivity` ignores any that are left.
	const frameWindows = new WeakSet<Window>();
	const onBlur = () => {
		setTimeout(() => {
			if (disposed || !document.hasFocus()) return;
			const el = document.activeElement;
			if (!(el instanceof HTMLIFrameElement)) return;
			onActivity();
			let win: Window | null = null;
			try {
				win = el.contentWindow;
				// Throws for a cross-origin frame; skip those.
				void win?.document;
			} catch {
				return;
			}
			if (!win || frameWindows.has(win)) return;
			frameWindows.add(win);
			try {
				listenOn(win);
			} catch {
				frameWindows.delete(win);
			}
		}, 0);
	};

	listenOn(window);
	window.addEventListener('blur', onBlur);
	window.addEventListener('focus', onWake);
	document.addEventListener('visibilitychange', onWake);

	return () => {
		disposed = true;
		unlisten?.();
		if (trailing !== null) clearTimeout(trailing);
		unlistenOn(window);
		window.removeEventListener('blur', onBlur);
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
