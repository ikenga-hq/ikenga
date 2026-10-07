// plans/pwa S4 §7: the app side of a notification tap.
//
// The service worker opens `<route>?push=<kind>&ref=<opaque ref>` (a new
// window) or posts `{type: 'ikenga:push-open', k, r}` to a focused one. Either
// way the app gets only a kind and an opaque id (W4) and fetches the details
// live, here, from the server:
//
// - `capturePushDeepLink` runs from `browser-entry.ts`, before boot can
//   rewrite the URL (`boot/primary.tsx` replaces it with `/remote` on a
//   phone): it moves `push` / `ref` into sessionStorage and strips them.
// - `resolvePushOpen` looks the thing up: an ask or invite row by id, a
//   pending pairing request by id. A row already resolved reads "Already
//   answered on another device".

import { create } from 'zustand';

import { accessPairPending } from '@/lib/access/client';
import { type NotificationRow, notificationsList } from '@/lib/tauri-cmd';

import { isPushOpenMessage, type PushMessage } from './sw-logic';

export { capturePushDeepLink, PUSH_OPEN_STORAGE_KEY, takePushDeepLink } from './deeplink-capture';

/** Taps on a notification while this window is already open. */
export function onPushOpenMessage(handler: (msg: PushMessage) => void): () => void {
	if (typeof navigator === 'undefined' || !('serviceWorker' in navigator)) return () => {};
	const listener = (e: MessageEvent) => {
		if (isPushOpenMessage(e.data)) handler({ k: e.data.k, r: e.data.r });
	};
	navigator.serviceWorker.addEventListener('message', listener);
	return () => navigator.serviceWorker.removeEventListener('message', listener);
}

/** What a tap turned out to point at, after a live lookup. */
export type PushOpenOutcome =
	| { kind: 'ask-open'; row: NotificationRow }
	| { kind: 'already-answered' }
	| { kind: 'invite'; row: NotificationRow | null }
	| { kind: 'pairing-pending' }
	| { kind: 'pairing-gone' }
	| { kind: 'route' };

export interface ResolveDeps {
	notificationsList: typeof notificationsList;
	accessPairPending: typeof accessPairPending;
}

const DEFAULT_DEPS: ResolveDeps = { notificationsList, accessPairPending };

function rowId(r: string): number | null {
	const m = /^n:(\d{1,15})$/.exec(r);
	return m ? Number(m[1]) : null;
}

export async function resolvePushOpen(
	msg: PushMessage,
	deps: ResolveDeps = DEFAULT_DEPS
): Promise<PushOpenOutcome> {
	switch (msg.k) {
		case 'permission': {
			const id = rowId(msg.r);
			if (id === null) return { kind: 'route' };
			const rows = await deps.notificationsList({ kinds: ['permission'], limit: 200 });
			const row = rows.find((n) => n.id === id);
			if (!row || row.resolvedAt) return { kind: 'already-answered' };
			return { kind: 'ask-open', row };
		}
		case 'invite': {
			const id = rowId(msg.r);
			const rows =
				id === null ? [] : await deps.notificationsList({ kinds: ['invite'], limit: 100 });
			return { kind: 'invite', row: rows.find((n) => n.id === id) ?? null };
		}
		case 'pairing': {
			const id = msg.r.startsWith('pair:') ? msg.r.slice(5) : null;
			const pending = await deps.accessPairPending();
			return pending.some((p) => p.pairingId === id && p.state === 'awaiting_host')
				? { kind: 'pairing-pending' }
				: { kind: 'pairing-gone' };
		}
		default:
			return { kind: 'route' };
	}
}

/** The one line a resolved tap shows, or `null` for nothing to say. */
export function pushOpenNotice(outcome: PushOpenOutcome): string | null {
	switch (outcome.kind) {
		case 'already-answered':
			return 'That approval was already answered on another device.';
		case 'pairing-gone':
			return 'That pairing request has already been answered or has expired.';
		default:
			return null;
	}
}

/** Event the notification bell listens for to open its popover. */
export const OPEN_NOTIFICATIONS_EVENT = 'ikenga:open-notifications';

interface PushOpenState {
	notice: string | null;
	setNotice: (notice: string | null) => void;
}

export const usePushOpenStore = create<PushOpenState>((set) => ({
	notice: null,
	setNotice: (notice) => set({ notice }),
}));

/** Act on one tap: look it up, then open the right surface or say why not. */
export async function handlePushOpen(
	msg: PushMessage,
	opts: { navigate?: (path: string) => void } = {},
	deps: ResolveDeps = DEFAULT_DEPS
): Promise<PushOpenOutcome> {
	const outcome = await resolvePushOpen(msg, deps).catch(
		(): PushOpenOutcome => ({ kind: 'route' })
	);
	usePushOpenStore.getState().setNotice(pushOpenNotice(outcome));
	if (outcome.kind === 'ask-open' || outcome.kind === 'invite') {
		window.dispatchEvent(new CustomEvent(OPEN_NOTIFICATIONS_EVENT));
	}
	if (opts.navigate) {
		if (outcome.kind === 'pairing-pending' || outcome.kind === 'pairing-gone') {
			opts.navigate('/settings/devices');
		} else if (msg.k.startsWith('run_')) {
			opts.navigate('/automations?view=runs');
		} else if (msg.k === 'update') {
			opts.navigate('/settings/about');
		} else if (msg.k === 'test') {
			opts.navigate('/settings/notifications');
		}
	}
	return outcome;
}

/**
 * Close shown notifications whose ask is already over (the inbox calls this
 * after it loads), so a phone doesn't keep "Approval needed" for an ask
 * answered elsewhere.
 */
export async function closeResolvedNotifications(rows: NotificationRow[]): Promise<number> {
	if (typeof navigator === 'undefined' || !('serviceWorker' in navigator)) return 0;
	const reg = await navigator.serviceWorker.getRegistration().catch(() => undefined);
	if (!reg) return 0;
	const shown = await reg.getNotifications().catch(() => [] as Notification[]);
	const open = new Set(rows.filter((r) => !r.resolvedAt).map((r) => `permission:n:${r.id}`));
	let closed = 0;
	for (const n of shown) {
		if (n.tag.startsWith('permission:n:') && !open.has(n.tag)) {
			n.close();
			closed++;
		}
	}
	return closed;
}
