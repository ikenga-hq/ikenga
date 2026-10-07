// The Ikenga service worker (plans/pwa S1, W1/W2). Served at `/sw.js` by the
// daemon (`server/static_files.rs`, `Cache-Control: no-cache`, no
// `Service-Worker-Allowed`), scope `/`. Never registered under Tauri.
//
// Built by `scripts/pwa/vite-plugin-sw.ts` with esbuild (iife), which inlines
// `__PRECACHE__` (this build's shell files) and `__BUILD_ID__` (a hash of that
// list). Any change to a precached file changes this script's bytes, which is
// what makes the browser install a new worker — and the new worker WAITS
// (no `skipWaiting` on install) until the page's "Reload to update" posts
// `SKIP_WAITING`.
//
// Every branching rule lives in `src/lib/pwa/sw-logic.ts` (unit-tested); this
// file only wires it to the worker events.

import {
	classify,
	clickUrl,
	isSkipWaitingMessage,
	notificationFor,
	PUSH_OPEN_MESSAGE,
	parsePush,
	RUNTIME_CACHE,
	RUNTIME_CACHE_MAX_ENTRIES,
	resubscribeBody,
	shellCacheName,
	shouldStore,
	staleShellCaches,
} from '../src/lib/pwa/sw-logic';

declare const self: ServiceWorkerGlobalScope;
declare const __PRECACHE__: string[];
declare const __BUILD_ID__: string;

const SHELL_CACHE = shellCacheName(__BUILD_ID__);
const PRECACHE = new Set<string>(__PRECACHE__);
const INDEX = '/index.html';

self.addEventListener('install', (event) => {
	event.waitUntil(
		(async () => {
			const cache = await caches.open(SHELL_CACHE);
			// `index.html` names the hashed bundles, so it must come from the
			// server, not a stale HTTP-cache copy. Hashed files are immutable;
			// the HTTP cache (warm from the first load) is as good as the wire.
			const requests = __PRECACHE__.map((path) =>
				path === INDEX ? new Request(path, { cache: 'reload' }) : new Request(path)
			);
			await cache.addAll(requests);
		})()
	);
});

self.addEventListener('activate', (event) => {
	event.waitUntil(
		(async () => {
			const names = await caches.keys();
			await Promise.all(staleShellCaches(names, __BUILD_ID__).map((n) => caches.delete(n)));
			await trimRuntimeCache();
			await self.clients.claim();
		})()
	);
});

self.addEventListener('message', (event) => {
	if (isSkipWaitingMessage(event.data)) void self.skipWaiting();
});

self.addEventListener('fetch', (event) => {
	const req = event.request;
	const route = classify(
		{
			url: req.url,
			method: req.method,
			mode: req.mode,
			hasAuthorization: req.headers.has('authorization'),
		},
		self.location.origin,
		PRECACHE
	);
	// Not ours: no `respondWith`, so the browser handles it untouched and the
	// worker never sees the response (W2 — no API response is ever cached).
	if (route === 'passthrough') return;

	if (route === 'navigation') {
		event.respondWith(navigation(req));
	} else if (route === 'precached-asset') {
		event.respondWith(precached(req));
	} else {
		event.respondWith(runtimeAsset(req));
	}
});

/** The cached shell for instant start; the network when there is none. */
async function navigation(req: Request): Promise<Response> {
	const cache = await caches.open(SHELL_CACHE);
	const shell = await cache.match(INDEX, MATCH);
	if (shell) return shell;
	return fetch(req);
}

// `ignoreVary`: the daemon's CORS layer answers `Vary: origin, …` and a
// module script request carries an `Origin` the precache request did not, so
// a Vary-respecting match misses every entry and the shell can't start
// offline. These are immutable build files: one URL, one body.
const MATCH: CacheQueryOptions = { ignoreVary: true };

async function precached(req: Request): Promise<Response> {
	const cache = await caches.open(SHELL_CACHE);
	const hit = await cache.match(req, MATCH);
	return hit ?? fetch(req);
}

async function runtimeAsset(req: Request): Promise<Response> {
	const cache = await caches.open(RUNTIME_CACHE);
	const hit = await cache.match(req, MATCH);
	if (hit) return hit;
	const res = await fetch(req);
	if (shouldStore(res)) {
		// Stored off the response path; a failed put never fails the load.
		void cache.put(req, res.clone()).catch(() => {});
	}
	return res;
}

/** Oldest-first eviction down to the bound (cache keys keep insertion order). */
async function trimRuntimeCache(): Promise<void> {
	const cache = await caches.open(RUNTIME_CACHE);
	const keys = await cache.keys();
	const excess = keys.length - RUNTIME_CACHE_MAX_ENTRIES;
	if (excess <= 0) return;
	await Promise.all(keys.slice(0, excess).map((k) => cache.delete(k)));
}

// ── Push (plans/pwa S4 §5–§6, W4) ───────────────────────────────────────────
//
// The browser decrypts; the payload is only `{v, k, r}`. Every push shows a
// notification (`userVisibleOnly`), with a title fixed per kind — never text
// from the payload — and a tap opens a same-origin URL built from the kind.

self.addEventListener('push', (event) => {
	let data: unknown = null;
	try {
		data = event.data?.json() ?? null;
	} catch {
		data = null;
	}
	const spec = notificationFor(parsePush(data));
	event.waitUntil(self.registration.showNotification(spec.title, spec.options));
});

self.addEventListener('notificationclick', (event) => {
	event.notification.close();
	const data = event.notification.data as { k?: string; r?: string } | null;
	const url = clickUrl(data);
	event.waitUntil(
		(async () => {
			const windows = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
			const own = windows.find((c) => new URL(c.url).origin === self.location.origin);
			if (own) {
				await own.focus();
				if (data?.k && data.r) own.postMessage({ type: PUSH_OPEN_MESSAGE, k: data.k, r: data.r });
				return;
			}
			await self.clients.openWindow(url);
		})()
	);
});

// The browser rotated the subscription. Re-subscribe with the same server key
// and tell the server, replacing the old endpoint. Cookie credentials only
// (a paired device or a T1 session); a T0 link-token tab has none here and is
// reconciled the next time the app opens.
self.addEventListener('pushsubscriptionchange', (event) => {
	const change = event as Event & {
		oldSubscription?: PushSubscription | null;
		newSubscription?: PushSubscription | null;
		waitUntil(p: Promise<unknown>): void;
	};
	change.waitUntil(
		(async () => {
			const old = change.oldSubscription ?? null;
			let sub = change.newSubscription ?? null;
			if (!sub) {
				const key = old?.options?.applicationServerKey;
				if (!key) return;
				sub = await self.registration.pushManager.subscribe({
					userVisibleOnly: true,
					applicationServerKey: key,
				});
			}
			const json = sub.toJSON() as { endpoint?: string; keys?: { p256dh?: string; auth?: string } };
			if (!json.endpoint || !json.keys?.p256dh || !json.keys.auth) return;
			await fetch('/api/rpc', {
				method: 'POST',
				credentials: 'same-origin',
				headers: { 'content-type': 'application/json' },
				body: resubscribeBody(
					{ endpoint: json.endpoint, keys: { p256dh: json.keys.p256dh, auth: json.keys.auth } },
					old?.endpoint ?? null
				),
			}).catch(() => {});
		})()
	);
});
