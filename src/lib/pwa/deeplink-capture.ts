// plans/pwa S4 §7: capture a notification tap's `?push=&ref=` before boot.
//
// Imported by `lib/transport/browser-entry.ts`, the app's FIRST import, so it
// must not pull in anything that reaches the transport at load time: it
// depends only on the pure `sw-logic` rules. Acting on the tap is
// `deeplink.ts`.

import { isPushKind, isValidPushRef, type PushMessage } from './sw-logic';

export const PUSH_OPEN_STORAGE_KEY = 'ikenga.pushOpen.v1';

/** Move `?push=&ref=` into sessionStorage and strip them from the URL. */
export function capturePushDeepLink(
	loc: Pick<Location, 'pathname' | 'search' | 'hash'> = window.location,
	history: Pick<History, 'replaceState'> = window.history
): PushMessage | null {
	const params = new URLSearchParams(loc.search);
	if (!params.has('push') && !params.has('ref')) return null;
	const k = params.get('push');
	const r = params.get('ref');
	params.delete('push');
	params.delete('ref');
	const rest = params.toString();
	// Runs before the router exists, so there is no history state to keep.
	history.replaceState(null, '', `${loc.pathname}${rest ? `?${rest}` : ''}${loc.hash}`);
	if (!isPushKind(k) || !isValidPushRef(r)) return null;
	const msg = { k, r };
	try {
		sessionStorage.setItem(PUSH_OPEN_STORAGE_KEY, JSON.stringify(msg));
	} catch {
		// Storage blocked: the tap still opened the right route.
	}
	return msg;
}

/** The captured tap, once. */
export function takePushDeepLink(): PushMessage | null {
	try {
		const raw = sessionStorage.getItem(PUSH_OPEN_STORAGE_KEY);
		sessionStorage.removeItem(PUSH_OPEN_STORAGE_KEY);
		if (!raw) return null;
		const v = JSON.parse(raw) as { k?: unknown; r?: unknown };
		return isPushKind(v.k) && isValidPushRef(v.r) ? { k: v.k, r: v.r } : null;
	} catch {
		return null;
	}
}
