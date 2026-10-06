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
