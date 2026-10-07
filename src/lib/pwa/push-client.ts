// plans/pwa S4 §4: turning push on and off for this browser, and keeping the
// browser's subscription and the server's row in step.
//
// - `enablePush` must run inside a click handler: it asks for permission
//   FIRST (Safari refuses a prompt that isn't in a user gesture), then makes
//   the subscription and registers it with the server.
// - Nothing asks for permission on load. `reconcilePush` (app start) only
//   repairs a subscription the user already turned on: a rotated server key,
//   or a server that lost the row.
// - The server never returns keys or the endpoint path; this module is the
//   only place that handles them.

import {
	accessPushConfig,
	accessPushList,
	accessPushSubscribe,
	accessPushUnsubscribe,
	type PushConfig,
	type PushKind,
} from '@/lib/access/client';

/** This browser's handle on its server row (per origin, survives reloads). */
export const SUB_STORAGE_KEY = 'ikenga.pushSub.v1';

export interface StoredSub {
	subId: string;
	endpoint: string;
	keyId: string;
}

export function readStoredSub(): StoredSub | null {
	try {
		const raw = localStorage.getItem(SUB_STORAGE_KEY);
		if (!raw) return null;
		const v = JSON.parse(raw) as Partial<StoredSub>;
		return typeof v.subId === 'string' &&
			typeof v.endpoint === 'string' &&
			typeof v.keyId === 'string'
			? { subId: v.subId, endpoint: v.endpoint, keyId: v.keyId }
			: null;
	} catch {
		return null;
	}
}

function writeStoredSub(v: StoredSub | null): void {
	try {
		if (v) localStorage.setItem(SUB_STORAGE_KEY, JSON.stringify(v));
		else localStorage.removeItem(SUB_STORAGE_KEY);
	} catch {
		// Private mode / blocked storage: the server list still works.
	}
}

export function b64urlToBytes(s: string): Uint8Array<ArrayBuffer> {
	const b64 = s.replace(/-/g, '+').replace(/_/g, '/');
	const pad = b64.length % 4 === 0 ? '' : '='.repeat(4 - (b64.length % 4));
	const bin = atob(b64 + pad);
	const out = new Uint8Array(new ArrayBuffer(bin.length));
	for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
	return out;
}

export function bytesToB64url(bytes: ArrayBuffer | Uint8Array): string {
	const u8 = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
	let bin = '';
	for (const b of u8) bin += String.fromCharCode(b);
	return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

/** Whether a subscription was made with `publicKey` (base64url). */
export function sameServerKey(sub: PushSubscription, publicKey: string): boolean {
	const key = sub.options?.applicationServerKey;
	if (!key) return false;
	return bytesToB64url(key) === publicKey.replace(/=+$/, '');
}

/** "Chrome on Android"-style label for the devices list (sanitized server-side). */
export function labelFromUserAgent(ua: string, platform = ''): string {
	const browser = /Edg\//.test(ua)
		? 'Edge'
		: /Firefox\/|FxiOS/.test(ua)
			? 'Firefox'
			: /Chrome\/|CriOS/.test(ua)
				? 'Chrome'
				: /Safari\//.test(ua)
					? 'Safari'
					: 'Browser';
	const os = /iPhone|iPod/.test(ua)
		? 'iPhone'
		: /iPad/.test(ua) || (platform === 'MacIntel' && /Mobile\//.test(ua))
			? 'iPad'
			: /Android/.test(ua)
				? 'Android'
				: /Windows/.test(ua)
					? 'Windows'
					: /Mac OS X|Macintosh/.test(ua)
						? 'Mac'
						: /Linux/.test(ua)
							? 'Linux'
							: 'this device';
	return `${browser} on ${os}`;
}

function subscriptionKeys(sub: PushSubscription): { p256dh: string; auth: string } {
	const json = sub.toJSON() as { keys?: { p256dh?: string; auth?: string } };
	const p256dh = json.keys?.p256dh;
	const auth = json.keys?.auth;
	if (!p256dh || !auth) throw new Error('The browser returned a subscription without keys.');
	return { p256dh, auth };
}

async function registration(): Promise<ServiceWorkerRegistration> {
	if (!('serviceWorker' in navigator)) throw new Error('This browser has no service worker.');
	// `ready` never settles without a registered worker (dev builds); don't hang.
	const timeout = new Promise<never>((_, reject) =>
		setTimeout(() => reject(new Error('The Ikenga service worker is not running yet.')), 10_000)
	);
	return Promise.race([navigator.serviceWorker.ready, timeout]);
}

async function subscribeWith(
	reg: ServiceWorkerRegistration,
	config: Extract<PushConfig, { enabled: true }>,
	kinds?: PushKind[]
): Promise<StoredSub> {
	const existing = await reg.pushManager.getSubscription();
	if (existing && !sameServerKey(existing, config.publicKey)) {
		await existing.unsubscribe().catch(() => false);
	}
	const sub =
		existing && sameServerKey(existing, config.publicKey)
			? existing
			: await reg.pushManager.subscribe({
					userVisibleOnly: true,
					applicationServerKey: b64urlToBytes(config.publicKey),
				});
	const { subId } = await accessPushSubscribe({
		endpoint: sub.endpoint,
		keys: subscriptionKeys(sub),
		...(kinds ? { kinds } : {}),
		label: labelFromUserAgent(navigator.userAgent, navigator.platform),
	});
	const stored = { subId, endpoint: sub.endpoint, keyId: config.keyId };
	writeStoredSub(stored);
	return stored;
}

export type EnableResult =
	| { ok: true; sub: StoredSub }
	| { ok: false; reason: 'denied' | 'off' | 'error'; message: string };

/**
 * Turn push on. Call from a click handler: the permission prompt is the first
 * thing it does.
 */
export async function enablePush(kinds?: PushKind[]): Promise<EnableResult> {
	let permission: NotificationPermission;
	try {
		permission = await Notification.requestPermission();
	} catch (e) {
		return { ok: false, reason: 'error', message: errorText(e) };
	}
	if (permission !== 'granted') {
		return {
			ok: false,
			reason: 'denied',
			message:
				permission === 'denied'
					? 'Notifications are blocked for this site. Allow them in the browser’s site settings.'
					: 'Notifications were not allowed.',
		};
	}
	try {
		const config = await accessPushConfig();
		if (!config.enabled) {
			return { ok: false, reason: 'off', message: config.reason ?? 'Push is off on this server.' };
		}
		const reg = await registration();
		return { ok: true, sub: await subscribeWith(reg, config, kinds) };
	} catch (e) {
		return { ok: false, reason: 'error', message: errorText(e) };
	}
}

/** Turn push off for this browser: the server row, then the subscription. */
export async function disablePush(): Promise<void> {
	const stored = readStoredSub();
	let sub: PushSubscription | null = null;
	try {
		const reg = await navigator.serviceWorker?.getRegistration();
		sub = (await reg?.pushManager.getSubscription()) ?? null;
	} catch {
		sub = null;
	}
	const target = stored ? { subId: stored.subId } : sub ? { endpoint: sub.endpoint } : null;
	if (target) await accessPushUnsubscribe(target).catch(() => undefined);
	await sub?.unsubscribe().catch(() => false);
	writeStoredSub(null);
}

/** Whether this browser currently holds a push subscription. */
export async function hasBrowserSubscription(): Promise<boolean> {
	try {
		const reg = await navigator.serviceWorker?.getRegistration();
		return Boolean(await reg?.pushManager.getSubscription());
	} catch {
		return false;
	}
}

/**
 * App start: repair a subscription the user already turned on. Never prompts.
 * Re-subscribes silently when the server key changed or the server no longer
 * has this browser's row; forgets the handle when the browser lost the
 * subscription.
 */
export async function reconcilePush(): Promise<'none' | 'ok' | 'repaired' | 'cleared'> {
	if (typeof Notification === 'undefined' || Notification.permission !== 'granted') return 'none';
	if (!('serviceWorker' in navigator)) return 'none';
	const stored = readStoredSub();
	const reg = await navigator.serviceWorker.getRegistration().catch(() => undefined);
	const sub = (await reg?.pushManager.getSubscription().catch(() => null)) ?? null;
	if (!stored && !sub) return 'none';
	if (!sub || !reg) {
		writeStoredSub(null);
		return 'cleared';
	}
	const config = await accessPushConfig().catch(() => null);
	if (!config?.enabled) return 'none';
	const rows = await accessPushList().catch(() => null);
	const known = rows?.some((r) => r.subId === stored?.subId) ?? true;
	const keyOk = stored?.keyId === config.keyId && sameServerKey(sub, config.publicKey);
	if (stored && known && keyOk && stored.endpoint === sub.endpoint) return 'ok';
	await subscribeWith(reg, config, rows?.find((r) => r.subId === stored?.subId)?.kinds);
	return 'repaired';
}

function errorText(e: unknown): string {
	if (e instanceof Error) return e.message;
	if (typeof e === 'string') return e.replace(/^[a-z_]+: /, '');
	return 'Something went wrong turning notifications on.';
}
