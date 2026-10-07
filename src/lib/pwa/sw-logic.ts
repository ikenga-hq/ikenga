// plans/pwa S1 (W2): every branch the service worker takes, DOM-free.
//
// `pwa/sw.ts` imports this and only wires it to the worker events, so the
// rules that decide what may ever be served from a cache are unit-tested here
// rather than trusted inside a worker nobody can step through.
//
// The rule (W2 "cached shell, live data"): the worker caches the built app
// shell and nothing else. Every request that carries or could carry identity —
// the RPC and WebSocket surfaces, auth and pairing, pkg bytes (served behind
// the auth layer), anything with an `Authorization` header, anything that is
// not a GET — is left entirely to the browser: the worker does not call
// `respondWith`, so it neither reads nor stores the response.

/** What the fetch handler does with one request. */
export type SwRoute =
	/** Not handled: the browser fetches it as if no worker existed. */
	| 'passthrough'
	/** An app navigation: the cached `index.html`, else the network. */
	| 'navigation'
	/** A file in this build's precache: cache first. */
	| 'precached-asset'
	/** A hashed `/assets/*` file outside the precache (a lazy route chunk):
	 *  cache first, stored after a successful same-origin fetch. */
	| 'runtime-asset';

/** The parts of a `Request` the classifier reads. */
export interface SwRequestInfo {
	url: string;
	method: string;
	/** `Request.mode` — `'navigate'` for a top-level document load. */
	mode: string;
	/** True when the request carries an `Authorization` header. */
	hasAuthorization: boolean;
}

/**
 * Path prefixes that are never served by the worker. Each is matched as the
 * whole path or as a directory (`/api` and `/api/…`, not `/apiary`).
 */
export const NETWORK_ONLY_PREFIXES = [
	'/api',
	'/ws',
	'/auth',
	'/access',
	'/pkgs',
	'/internal',
	'/__viewer',
	'/__viewer-health',
] as const;

/** The worker script itself: the browser's update check must reach the server. */
const SW_PATH = '/sw.js';

export const SHELL_CACHE_PREFIX = 'ikenga-shell-';
export const RUNTIME_CACHE = 'ikenga-assets-runtime';
/** Bound on the runtime cache — lazy chunks of old builds age out. */
export const RUNTIME_CACHE_MAX_ENTRIES = 400;

export function shellCacheName(buildId: string): string {
	return `${SHELL_CACHE_PREFIX}${buildId}`;
}

/** True for a path the worker must never answer. */
export function isNetworkOnlyPath(pathname: string): boolean {
	if (pathname === SW_PATH) return true;
	return NETWORK_ONLY_PREFIXES.some((p) => pathname === p || pathname.startsWith(`${p}/`));
}

/**
 * Decide how the worker handles a request. `origin` is the worker's own
 * origin; `precache` holds URL paths (`/assets/app-1a2b.js`).
 */
export function classify(
	req: SwRequestInfo,
	origin: string,
	precache: ReadonlySet<string>
): SwRoute {
	if (req.method.toUpperCase() !== 'GET') return 'passthrough';
	if (req.hasAuthorization) return 'passthrough';

	let url: URL;
	try {
		url = new URL(req.url);
	} catch {
		return 'passthrough';
	}
	if (url.origin !== origin) return 'passthrough';
	if (isNetworkOnlyPath(url.pathname)) return 'passthrough';

	if (req.mode === 'navigate') return 'navigation';
	// A query string never names a different build file; it does name a
	// different thing for anything dynamic, which is not cached anyway.
	if (precache.has(url.pathname) && url.search === '') return 'precached-asset';
	if (url.pathname.startsWith('/assets/') && url.search === '') return 'runtime-asset';
	return 'passthrough';
}

/** The parts of a `Response` the store decision reads. */
export interface SwResponseInfo {
	ok: boolean;
	status: number;
	type: string;
}

/**
 * Only a same-origin (`basic`), successful, complete response is stored — never
 * an opaque, redirected-error or partial (206) one.
 */
export function shouldStore(res: SwResponseInfo): boolean {
	return res.ok && res.status === 200 && res.type === 'basic';
}

/** Shell caches from older builds, to delete once this build activates. */
export function staleShellCaches(names: readonly string[], buildId: string): string[] {
	const current = shellCacheName(buildId);
	return names.filter((n) => n.startsWith(SHELL_CACHE_PREFIX) && n !== current);
}

/** The single message the page sends the worker. */
export const SKIP_WAITING_MESSAGE = { type: 'SKIP_WAITING' } as const;

export function isSkipWaitingMessage(data: unknown): boolean {
	return (
		typeof data === 'object' &&
		data !== null &&
		(data as { type?: unknown }).type === SKIP_WAITING_MESSAGE.type
	);
}

// ── Push (plans/pwa S4 §5–§6, W4) ────────────────────────────────────────────
//
// The server sends only `{v: 1, k: <kind>, r: <opaque ref>}`. Titles are fixed
// per kind here and click targets are built from the kind enum, never from
// payload text: a payload can't put words on the lock screen or send a tap to
// another site.

export const PUSH_KINDS = [
	'permission',
	'run_finished',
	'run_failed',
	'run_cancelled',
	'invite',
	'pairing',
	'update',
	'test',
] as const;

export type PushWireKind = (typeof PUSH_KINDS)[number];

export interface PushMessage {
	k: PushWireKind;
	r: string;
}

const PUSH_TITLES: Record<PushWireKind, string> = {
	permission: 'Approval needed in Ikenga',
	run_finished: 'Run finished',
	run_failed: 'Run failed',
	run_cancelled: 'Run cancelled',
	invite: 'Someone joined from your invite',
	pairing: 'A device is asking to pair',
	update: 'Server update available',
	test: 'Notifications are on',
};

/** Where a tap on each kind lands. */
const PUSH_ROUTES: Record<PushWireKind, string> = {
	permission: '/',
	run_finished: '/automations?view=runs',
	run_failed: '/automations?view=runs',
	run_cancelled: '/automations?view=runs',
	invite: '/settings/members',
	pairing: '/settings/devices',
	update: '/settings/about',
	test: '/settings/notifications',
};

export const GENERIC_PUSH_TITLE = 'Ikenga needs your attention';
export const PUSH_BODY = 'Open Ikenga to see details';

/** `r`: opaque, ≤64, `[A-Za-z0-9:_-]` (the server's own check). */
export function isValidPushRef(r: unknown): r is string {
	return typeof r === 'string' && /^[A-Za-z0-9:_-]{1,64}$/.test(r);
}

export function isPushKind(k: unknown): k is PushWireKind {
	return typeof k === 'string' && (PUSH_KINDS as readonly string[]).includes(k);
}

/** A decrypted push payload → a message, or `null` when it isn't ours. */
export function parsePush(data: unknown): PushMessage | null {
	if (typeof data !== 'object' || data === null) return null;
	const { v, k, r } = data as { v?: unknown; k?: unknown; r?: unknown };
	if (v !== 1 || !isPushKind(k) || !isValidPushRef(r)) return null;
	return { k, r };
}

export interface PushNotificationSpec {
	title: string;
	options: {
		body: string;
		icon: string;
		badge: string;
		tag: string;
		renotify?: boolean;
		requireInteraction?: boolean;
		data: { k: PushWireKind; r: string } | null;
	};
}

/**
 * What to show for a push. `userVisibleOnly` means every push must show a
 * notification, so an unknown payload still gets the generic one.
 */
export function notificationFor(msg: PushMessage | null): PushNotificationSpec {
	const base = { body: PUSH_BODY, icon: '/icons/icon-192.png', badge: '/icons/badge-72.png' };
	if (!msg) {
		return { title: GENERIC_PUSH_TITLE, options: { ...base, tag: 'ikenga', data: null } };
	}
	const spec: PushNotificationSpec = {
		title: PUSH_TITLES[msg.k],
		options: { ...base, tag: `${msg.k}:${msg.r}`, data: { k: msg.k, r: msg.r } },
	};
	if (msg.k === 'permission') {
		spec.options.renotify = true;
		spec.options.requireInteraction = true;
	}
	return spec;
}

/** The same-origin path a tap opens: the kind's route plus `push` / `ref`. */
export function clickUrl(data: unknown): string {
	const msg =
		typeof data === 'object' && data !== null
			? parsePush({ v: 1, ...(data as Record<string, unknown>) })
			: null;
	if (!msg) return '/';
	const route = PUSH_ROUTES[msg.k];
	const sep = route.includes('?') ? '&' : '?';
	return `${route}${sep}push=${msg.k}&ref=${encodeURIComponent(msg.r)}`;
}

/** The message the worker posts to an open window on a tap. */
export const PUSH_OPEN_MESSAGE = 'ikenga:push-open';

export function isPushOpenMessage(data: unknown): data is { type: string } & PushMessage {
	if (typeof data !== 'object' || data === null) return false;
	const d = data as { type?: unknown; k?: unknown; r?: unknown };
	return d.type === PUSH_OPEN_MESSAGE && isPushKind(d.k) && isValidPushRef(d.r);
}

/** The `/api/rpc` body the worker sends from `pushsubscriptionchange`: the new
 *  subscription, replacing the old endpoint (whose kinds and label the server
 *  carries over). Cookie credentials only — a worker has no bearer token. */
export function resubscribeBody(
	sub: { endpoint: string; keys: { p256dh: string; auth: string } },
	oldEndpoint: string | null
): string {
	return JSON.stringify({
		cmd: 'access_push_subscribe',
		args: {
			endpoint: sub.endpoint,
			keys: sub.keys,
			...(oldEndpoint ? { replaces: oldEndpoint } : {}),
		},
	});
}
