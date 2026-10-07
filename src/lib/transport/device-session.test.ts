// WP-74b: the paired-device boot probe (G-ACCESS §2.4, P-21).

import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	_resetDeviceSessionForTests,
	bootsIntoRemote,
	detectAccessStatus,
	isDeviceSession,
	UNREACHABLE,
} from './device-session';

afterEach(() => {
	_resetDeviceSessionForTests();
	vi.unstubAllGlobals();
});

function stubFetch(status: number, body: unknown) {
	const f = vi.fn(async () => ({ ok: status === 200, status, json: async () => body }));
	vi.stubGlobal('fetch', f);
	return f;
}

describe('detectAccessStatus', () => {
	it('marks a device session from the cookie alone and boots a phone into /remote', async () => {
		const f = stubFetch(200, {
			ok: true,
			data: {
				tier: 't0',
				credential: { via: 'device', deviceId: 'd', tier: 'dispatch' },
				caps: [],
				adminStrength: false,
			},
		});
		const s = await detectAccessStatus();
		expect(isDeviceSession()).toBe(true);
		expect(bootsIntoRemote(s)).toBe(true);
		const init = (f.mock.calls[0] as unknown as [string, RequestInit])[1];
		expect(init.credentials).toBe('same-origin');
		expect((init.headers as Record<string, string>).Authorization).toBeUndefined();
	});

	it('a full device or the operator gets the full shell; a 401 is no session', async () => {
		stubFetch(200, {
			ok: true,
			data: {
				tier: 't0',
				credential: { via: 'device', deviceId: 'd', tier: 'full' },
				caps: [],
				adminStrength: true,
			},
		});
		expect(bootsIntoRemote(await detectAccessStatus())).toBe(false);
		stubFetch(200, {
			ok: true,
			data: {
				tier: 't0',
				credential: { via: 'operator', deviceId: 'h', tier: 'full' },
				caps: [],
				adminStrength: true,
			},
		});
		expect(bootsIntoRemote(await detectAccessStatus('tok'))).toBe(false);
		_resetDeviceSessionForTests();
		stubFetch(401, {});
		expect(await detectAccessStatus()).toBeNull();
		expect(isDeviceSession()).toBe(false);
	});

	// plans/pwa S1 (W2): no server at all is not "no credential".
	it('reports an unreachable server distinctly from a missing credential', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				throw new TypeError('Failed to fetch');
			})
		);
		expect(await detectAccessStatus()).toBe(UNREACHABLE);
		expect(bootsIntoRemote(UNREACHABLE)).toBe(false);
		expect(isDeviceSession()).toBe(false);

		for (const gateway of [502, 503, 504]) {
			stubFetch(gateway, {});
			expect(await detectAccessStatus()).toBe(UNREACHABLE);
		}

		// A malformed body from a server that DID answer is still "no session".
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => ({
				ok: true,
				status: 200,
				json: async () => {
					throw new SyntaxError('bad json');
				},
			}))
		);
		expect(await detectAccessStatus()).toBeNull();
	});

	it('treats any failure while the browser is offline as unreachable', async () => {
		vi.stubGlobal('navigator', { ...navigator, onLine: false });
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				throw new Error('aborted');
			})
		);
		expect(await detectAccessStatus()).toBe(UNREACHABLE);
	});
});
