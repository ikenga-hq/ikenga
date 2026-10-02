// WP-74b: the device side of pairing against a TS stand-in for the host
// (`startB`, the same construction the Rust host runs — A-13 pins them to
// each other), plus the code and UA helpers.

import { describe, expect, it, vi } from 'vitest';

import { fingerprintPhrase } from '@/lib/access/fingerprint';
import { displayPairCode, normalizePairCode } from '@/lib/access/pair-code';
import {
	b64url,
	deriveKeys,
	hostIdentity,
	ID_A,
	pairPassword,
	startB,
	unb64url,
	utf8,
} from '@/lib/access/spake2';

import { codeFromHash, deviceNameFromUA, runPairing } from './pair-flow';

describe('pairing code (§3.2)', () => {
	it('normalizes typed input', () => {
		expect(normalizePairCode('k7p-42q')).toBe('K7P42Q');
		expect(normalizePairCode(' o1l iab ')).toBe('0111AB');
		expect(normalizePairCode('K7P-42')).toBeNull();
		expect(normalizePairCode('K7U-42Q')).toBeNull();
		expect(displayPairCode('K7P42Q')).toBe('K7P-42Q');
	});

	it('reads the QR fragment', () => {
		expect(codeFromHash('#c=K7P42Q')).toBe('K7P42Q');
		expect(codeFromHash('#c=k7p-42q')).toBe('K7P42Q');
		expect(codeFromHash('')).toBeNull();
	});

	it('names the device from its UA', () => {
		const pixel =
			'Mozilla/5.0 (Linux; Android 14; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0 Mobile Safari/537.36';
		expect(deviceNameFromUA(pixel)).toEqual({ name: 'Pixel 9 · Chrome', platform: 'android' });
		const iphone =
			'Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1';
		expect(deviceNameFromUA(iphone)).toEqual({ name: 'iPhone · Safari', platform: 'ios' });
	});
});

/** A fake host: the §3.4/§3.5 host side over `fetch`. */
function fakeHost(opts: {
	code: string;
	storeId?: string;
	statuses?: string[];
	helloStatus?: number;
}) {
	const storeId = opts.storeId ?? 'store-1';
	const pairingId = 'pid-1';
	let keys: ReturnType<typeof deriveKeys> | null = null;
	let words: string[] | null = null;
	const statuses = [...(opts.statuses ?? ['awaiting_host', 'allowed'])];
	const calls: string[] = [];
	const res = (status: number, body: unknown) =>
		({ status, json: async () => body }) as unknown as Response;
	const f = vi.fn(async (path: string, init?: RequestInit) => {
		calls.push(path.split('?')[0] ?? path);
		const body = init?.body ? JSON.parse(String(init.body)) : {};
		if (path === '/access/pair/hello') {
			if (opts.helloStatus) return res(opts.helloStatus, { ok: false, retry_after_ms: 30000 });
			const msgA = unb64url(body.msgA);
			const b = startB(pairPassword(opts.code), utf8(ID_A), hostIdentity(storeId));
			const key = b.finish(msgA);
			keys = deriveKeys(key, pairingId, msgA, b.msg);
			words = fingerprintPhrase(keys.fpSeed);
			return res(200, {
				ok: true,
				pairingId,
				msgB: b64url(b.msg),
				hostConfirm: b64url(keys.hostConfirm),
				storeId,
			});
		}
		if (path === '/access/pair/confirm') {
			const ok = keys && b64url(keys.deviceConfirm) === body.deviceConfirm;
			return ok
				? res(200, { ok: true, state: 'awaiting_host' })
				: res(404, { ok: false, error: 'pair_failed' });
		}
		const poll = ((init?.headers ?? {}) as Record<string, string>)['X-Ikenga-Pair-Poll'];
		if (!keys || poll !== b64url(keys.pollKey)) return res(404, { ok: false });
		const state = statuses.shift() ?? 'awaiting_host';
		return res(
			200,
			state === 'allowed'
				? { ok: true, state, device_id: 'd1', tier: 'dispatch' }
				: { ok: true, state }
		);
	});
	return { f: f as unknown as typeof fetch, calls, words: () => words };
}

const device = { name: 'Pixel 9 · Chrome', platform: 'android' };
const fast = { sleep: async () => {}, pollEveryMs: 0 };

describe('runPairing', () => {
	it('pairs: same words as the host, cookie delivered on allow', async () => {
		const host = fakeHost({ code: 'K7P42Q' });
		const onWords = vi.fn();
		const out = await runPairing('K7P42Q', device, { onWords }, { fetch: host.f, ...fast });
		expect(out).toEqual({ kind: 'allowed', deviceId: 'd1', tier: 'dispatch' });
		expect(onWords).toHaveBeenCalledWith(host.words());
		expect(host.calls).toEqual([
			'/access/pair/hello',
			'/access/pair/confirm',
			'/access/pair/status',
			'/access/pair/status',
		]);
	});

	it('a wrong code fails on the device and still burns the code on the host', async () => {
		const host = fakeHost({ code: 'K7P42Q' });
		const onWords = vi.fn();
		const out = await runPairing('K7P42R', device, { onWords }, { fetch: host.f, ...fast });
		expect(out.kind).toBe('failed');
		expect(onWords).not.toHaveBeenCalled();
		expect(host.calls).toEqual(['/access/pair/hello', '/access/pair/confirm']);
	});

	it('maps the host outcomes', async () => {
		for (const state of ['denied', 'expired', 'burned', 'cancelled'] as const) {
			const host = fakeHost({ code: 'K7P42Q', statuses: ['awaiting_host', state] });
			const out = await runPairing('K7P42Q', device, {}, { fetch: host.f, ...fast });
			expect(out.kind).toBe(state);
		}
		const throttled = fakeHost({ code: 'K7P42Q', helloStatus: 429 });
		expect(await runPairing('K7P42Q', device, {}, { fetch: throttled.f, ...fast })).toEqual({
			kind: 'throttled',
			retryAfterMs: 30000,
		});
		const unreachable = vi.fn(async () => {
			throw new TypeError('network');
		}) as unknown as typeof fetch;
		expect((await runPairing('K7P42Q', device, {}, { fetch: unreachable, ...fast })).kind).toBe(
			'unreachable'
		);
	});
});
