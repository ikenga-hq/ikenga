// plans/pwa S1 (W2): the service worker's routing rules. The security-critical
// half is the `passthrough` set — nothing that carries identity or reaches the
// API may ever be answered (or stored) by the worker.

import { describe, expect, it } from 'vitest';
import {
	classify,
	isNetworkOnlyPath,
	isSkipWaitingMessage,
	RUNTIME_CACHE,
	SKIP_WAITING_MESSAGE,
	type SwRequestInfo,
	shellCacheName,
	shouldStore,
	staleShellCaches,
} from './sw-logic';

const ORIGIN = 'https://ikenga.example.ts.net';
const PRECACHE = new Set(['/index.html', '/assets/index-abc123.js', '/icons/icon-192.png']);

function req(path: string, over: Partial<SwRequestInfo> = {}): SwRequestInfo {
	return {
		url: path.startsWith('http') ? path : `${ORIGIN}${path}`,
		method: 'GET',
		mode: 'cors',
		hasAuthorization: false,
		...over,
	};
}

describe('classify', () => {
	it('never handles the API, sockets, auth, access, pkgs or internal routes', () => {
		for (const path of [
			'/api/rpc',
			'/api/health',
			'/api',
			'/ws/pty/abc',
			'/ws/fs',
			'/auth/login',
			'/auth/me',
			'/access/pair/start',
			'/pkgs/com.ikenga.tasks/',
			'/pkgs/com.ikenga.tasks/assets/app.js',
			'/internal/push/events',
			'/__viewer/x.html',
			'/sw.js',
		]) {
			expect(classify(req(path), ORIGIN, PRECACHE), path).toBe('passthrough');
			// Not even as a navigation: a top-level load of an API URL must hit
			// the server, never the cached shell.
			expect(classify(req(path, { mode: 'navigate' }), ORIGIN, PRECACHE), path).toBe('passthrough');
		}
	});

	it('matches network-only prefixes as whole segments only', () => {
		expect(isNetworkOnlyPath('/api')).toBe(true);
		expect(isNetworkOnlyPath('/api/')).toBe(true);
		expect(isNetworkOnlyPath('/apiary')).toBe(false);
		expect(isNetworkOnlyPath('/authors')).toBe(false);
		expect(isNetworkOnlyPath('/pkgsx')).toBe(false);
	});

	it('never handles a request with an Authorization header, even for a shell file', () => {
		for (const path of ['/assets/index-abc123.js', '/index.html', '/']) {
			expect(classify(req(path, { hasAuthorization: true }), ORIGIN, PRECACHE)).toBe('passthrough');
			expect(
				classify(req(path, { hasAuthorization: true, mode: 'navigate' }), ORIGIN, PRECACHE)
			).toBe('passthrough');
		}
	});

	it('never handles a non-GET request', () => {
		for (const method of ['POST', 'PUT', 'PATCH', 'DELETE', 'HEAD', 'OPTIONS']) {
			expect(classify(req('/assets/index-abc123.js', { method }), ORIGIN, PRECACHE)).toBe(
				'passthrough'
			);
			expect(classify(req('/', { method, mode: 'navigate' }), ORIGIN, PRECACHE)).toBe(
				'passthrough'
			);
		}
	});

	it('never handles another origin', () => {
		expect(classify(req('https://fonts.gstatic.com/s/inter.woff2'), ORIGIN, PRECACHE)).toBe(
			'passthrough'
		);
		expect(classify(req('https://evil.example/assets/index-abc123.js'), ORIGIN, PRECACHE)).toBe(
			'passthrough'
		);
		expect(classify(req('not a url'), ORIGIN, PRECACHE)).toBe('passthrough');
	});

	it('serves app navigations from the shell, query and deep path included', () => {
		for (const path of ['/', '/remote', '/sessions/abc', '/settings/devices?tab=1']) {
			expect(classify(req(path, { mode: 'navigate' }), ORIGIN, PRECACHE), path).toBe('navigation');
		}
	});

	it('serves precached files cache-first and other hashed assets from the runtime cache', () => {
		expect(classify(req('/assets/index-abc123.js'), ORIGIN, PRECACHE)).toBe('precached-asset');
		expect(classify(req('/icons/icon-192.png'), ORIGIN, PRECACHE)).toBe('precached-asset');
		expect(classify(req('/assets/route-lazy-9f9f.js'), ORIGIN, PRECACHE)).toBe('runtime-asset');
		// A query string makes it something else; leave it to the network.
		expect(classify(req('/assets/index-abc123.js?x=1'), ORIGIN, PRECACHE)).toBe('passthrough');
		// Unhashed public files are not cached at runtime.
		expect(classify(req('/install-catalog.json'), ORIGIN, PRECACHE)).toBe('passthrough');
	});
});

describe('shouldStore', () => {
	it('stores only complete, successful, same-origin responses', () => {
		expect(shouldStore({ ok: true, status: 200, type: 'basic' })).toBe(true);
		expect(shouldStore({ ok: false, status: 0, type: 'opaque' })).toBe(false);
		expect(shouldStore({ ok: true, status: 200, type: 'cors' })).toBe(false);
		expect(shouldStore({ ok: true, status: 206, type: 'basic' })).toBe(false);
		expect(shouldStore({ ok: false, status: 404, type: 'basic' })).toBe(false);
		expect(shouldStore({ ok: false, status: 401, type: 'basic' })).toBe(false);
	});
});

describe('cache names and messages', () => {
	it('deletes only older shell caches, never the runtime or foreign caches', () => {
		const names = [
			shellCacheName('aaaaaaaaaaaa'),
			shellCacheName('bbbbbbbbbbbb'),
			RUNTIME_CACHE,
			'some-other-app',
		];
		expect(staleShellCaches(names, 'bbbbbbbbbbbb')).toEqual([shellCacheName('aaaaaaaaaaaa')]);
	});

	it('recognises only the SKIP_WAITING message', () => {
		expect(isSkipWaitingMessage(SKIP_WAITING_MESSAGE)).toBe(true);
		expect(isSkipWaitingMessage({ type: 'skip_waiting' })).toBe(false);
		expect(isSkipWaitingMessage('SKIP_WAITING')).toBe(false);
		expect(isSkipWaitingMessage(null)).toBe(false);
	});
});
