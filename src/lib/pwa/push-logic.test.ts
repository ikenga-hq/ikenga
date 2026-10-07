// plans/pwa S4 (W4): what the service worker shows and opens for a push, what
// the browser can do about push, and the deep-link capture. The security
// half: titles are fixed per kind, click URLs come from the kind enum, and
// nothing from the payload reaches the screen or the address bar.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { capturePushDeepLink, PUSH_OPEN_STORAGE_KEY, takePushDeepLink } from './deeplink-capture';
import { type PushEnv, pushMessage, pushState } from './platform';
import {
	clickUrl,
	GENERIC_PUSH_TITLE,
	isPushOpenMessage,
	notificationFor,
	PUSH_BODY,
	PUSH_KINDS,
	PUSH_OPEN_MESSAGE,
	parsePush,
	resubscribeBody,
} from './sw-logic';

describe('push payload → notification', () => {
	it('accepts only {v: 1, k: <known kind>, r: <opaque ref>}', () => {
		expect(parsePush({ v: 1, k: 'permission', r: 'n:12' })).toEqual({ k: 'permission', r: 'n:12' });
		for (const bad of [
			null,
			'x',
			{ v: 2, k: 'permission', r: 'n:1' },
			{ v: 1, k: 'shell', r: 'n:1' },
			{ v: 1, k: 'permission', r: 'n 1' },
			{ v: 1, k: 'permission', r: 'https://evil.example/' },
			{ v: 1, k: 'permission', r: 'a'.repeat(65) },
			{ v: 1, k: 'permission' },
		]) {
			expect(parsePush(bad)).toBeNull();
		}
	});

	it('uses a fixed title per kind and never payload text', () => {
		const titles = new Set<string>();
		for (const k of PUSH_KINDS) {
			const spec = notificationFor({ k, r: 'n:1' });
			titles.add(spec.title);
			expect(spec.options.body).toBe(PUSH_BODY);
			expect(spec.options.tag).toBe(`${k}:n:1`);
			expect(spec.options.icon).toBe('/icons/icon-192.png');
		}
		expect(titles.size).toBe(PUSH_KINDS.length);
		expect(notificationFor({ k: 'permission', r: 'n:1' }).title).toBe('Approval needed in Ikenga');
		expect(notificationFor({ k: 'permission', r: 'n:1' }).options.requireInteraction).toBe(true);
		expect(
			notificationFor({ k: 'run_failed', r: 'run:x' }).options.requireInteraction
		).toBeUndefined();
		// Junk (or a payload with extra text) still shows — generic, nothing echoed.
		const junk = notificationFor(parsePush({ v: 1, k: 'permission', r: 'rm -rf /' }));
		expect(junk.title).toBe(GENERIC_PUSH_TITLE);
		expect(JSON.stringify(junk)).not.toContain('rm -rf');
	});

	it('builds click URLs from the kind enum, same-origin only', () => {
		expect(clickUrl({ k: 'permission', r: 'n:12' })).toBe('/?push=permission&ref=n%3A12');
		expect(clickUrl({ k: 'run_failed', r: 'run:abc' })).toBe(
			'/automations?view=runs&push=run_failed&ref=run%3Aabc'
		);
		expect(clickUrl({ k: 'pairing', r: 'pair:p1' })).toBe(
			'/settings/devices?push=pairing&ref=pair%3Ap1'
		);
		expect(clickUrl({ k: 'update', r: 'update' })).toBe('/settings/about?push=update&ref=update');
		for (const bad of [null, {}, { k: 'x', r: 'n:1' }, { k: 'permission', r: '//evil.example' }]) {
			expect(clickUrl(bad)).toBe('/');
		}
		for (const k of PUSH_KINDS) expect(clickUrl({ k, r: 'n:1' }).startsWith('/')).toBe(true);
		expect(clickUrl({ k: 'invite', r: 'n:1' }).startsWith('//')).toBe(false);
	});

	it('recognises only well-formed open messages', () => {
		expect(isPushOpenMessage({ type: PUSH_OPEN_MESSAGE, k: 'invite', r: 'n:3' })).toBe(true);
		expect(isPushOpenMessage({ type: PUSH_OPEN_MESSAGE, k: 'invite', r: '../x' })).toBe(false);
		expect(isPushOpenMessage({ type: 'other', k: 'invite', r: 'n:3' })).toBe(false);
	});

	it('re-subscribes replacing the old endpoint', () => {
		const body = JSON.parse(
			resubscribeBody(
				{ endpoint: 'https://fcm.googleapis.com/n', keys: { p256dh: 'p', auth: 'a' } },
				'https://fcm.googleapis.com/o'
			)
		);
		expect(body).toEqual({
			cmd: 'access_push_subscribe',
			args: {
				endpoint: 'https://fcm.googleapis.com/n',
				keys: { p256dh: 'p', auth: 'a' },
				replaces: 'https://fcm.googleapis.com/o',
			},
		});
		expect(
			JSON.parse(resubscribeBody({ endpoint: 'e', keys: { p256dh: 'p', auth: 'a' } }, null)).args
		).not.toHaveProperty('replaces');
	});
});

describe('pushState', () => {
	const ready: PushEnv = {
		isTauri: false,
		secureContext: true,
		standalone: false,
		userAgent: 'Mozilla/5.0 (Linux; Android 14) Chrome/130',
		platform: 'Linux armv8l',
		maxTouchPoints: 5,
		hasServiceWorker: true,
		hasPushManager: true,
		permission: 'default',
	};
	const iphone = 'Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) Safari/604.1';

	it('names exactly one state, in priority order', () => {
		expect(pushState(ready)).toBe('ready');
		expect(pushState({ ...ready, isTauri: true, secureContext: false })).toBe('tauri');
		expect(pushState({ ...ready, secureContext: false })).toBe('insecure');
		// An iOS Safari tab: install first (it has no PushManager either).
		expect(pushState({ ...ready, userAgent: iphone, hasPushManager: false })).toBe(
			'ios-needs-install'
		);
		// iPadOS presenting as a Mac.
		expect(
			pushState({
				...ready,
				userAgent: 'Macintosh Safari',
				platform: 'MacIntel',
				maxTouchPoints: 5,
			})
		).toBe('ios-needs-install');
		// Installed to the Home Screen on 16.4+: ready.
		expect(pushState({ ...ready, userAgent: iphone, standalone: true })).toBe('ready');
		// Installed on an older iOS: no PushManager.
		expect(
			pushState({ ...ready, userAgent: iphone, standalone: true, hasPushManager: false })
		).toBe('unsupported');
		expect(pushState({ ...ready, permission: 'unsupported' })).toBe('unsupported');
		expect(pushState({ ...ready, permission: 'denied' })).toBe('denied');
	});

	it('explains each state, and says install-and-pair on iOS', () => {
		expect(pushMessage('ready')).toBeNull();
		expect(pushMessage('ios-needs-install')).toMatch(/Add to Home Screen/);
		expect(pushMessage('ios-needs-install')).toMatch(/16\.4/);
		expect(pushMessage('ios-needs-install')).toMatch(/pair this device inside the installed app/);
		expect(pushMessage('insecure')).toMatch(/HTTPS/);
		expect(pushMessage('tauri')).toMatch(/browser/);
	});
});

describe('deep-link capture', () => {
	beforeEach(() => sessionStorage.clear());
	afterEach(() => sessionStorage.clear());

	it('moves push/ref into sessionStorage and strips them, keeping the rest', () => {
		const replaceState = vi.fn();
		const msg = capturePushDeepLink(
			{ pathname: '/automations', search: '?view=runs&push=run_failed&ref=run%3Aabc', hash: '#x' },
			{ replaceState }
		);
		expect(msg).toEqual({ k: 'run_failed', r: 'run:abc' });
		expect(replaceState).toHaveBeenCalledWith(null, '', '/automations?view=runs#x');
		expect(takePushDeepLink()).toEqual({ k: 'run_failed', r: 'run:abc' });
		expect(takePushDeepLink()).toBeNull();
	});

	it('strips but never stores a malformed tap', () => {
		const replaceState = vi.fn();
		expect(
			capturePushDeepLink(
				{ pathname: '/', search: '?push=permission&ref=javascript:alert(1)', hash: '' },
				{ replaceState }
			)
		).toBeNull();
		expect(replaceState).toHaveBeenCalledWith(null, '', '/');
		expect(sessionStorage.getItem(PUSH_OPEN_STORAGE_KEY)).toBeNull();
	});

	it('leaves a URL without push params alone', () => {
		const replaceState = vi.fn();
		expect(
			capturePushDeepLink({ pathname: '/x', search: '?a=1', hash: '' }, { replaceState })
		).toBeNull();
		expect(replaceState).not.toHaveBeenCalled();
	});
});
