// WP-74b: the paired-device boot probe (G-ACCESS §2.4, P-21).

import { afterEach, describe, expect, it, vi } from 'vitest';

import {
	_resetDeviceSessionForTests,
	bootsIntoRemote,
	detectAccessStatus,
	isDeviceSession,
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
});
