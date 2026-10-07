import { afterEach, describe, expect, it, vi } from 'vitest';
import { seatErrorOf } from '@/lib/queries/seats';
import { clearAuthToken, WebRemoteTransport } from './index';

// The daemon answers a typed refusal (the seats' `SeatError`) with the
// serialized object as `error_data` next to the `error` string. The web
// transport must reject with those fields on the Error, so the frontend reads
// the same rejection a Tauri `invoke` gives (`seatErrorOf`).

function answer(body: unknown) {
	global.fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => body });
}

describe('WebRemoteTransport typed rejections', () => {
	const originalFetch = global.fetch;
	afterEach(() => {
		global.fetch = originalFetch;
		clearAuthToken();
	});

	it('puts error_data on the thrown Error, readable by seatErrorOf', async () => {
		answer({
			ok: false,
			error: 'seats_clear: seat_held: lead is held by cli since 1',
			error_data: {
				code: 'seat_held',
				message: 'lead is held by cli since 1',
				details: { client: 'cli', since: 1, expires_at: 2 },
			},
		});
		const err = await new WebRemoteTransport()
			.invoke('seats_clear', { seatId: 's' })
			.catch((e) => e);
		expect(err).toBeInstanceOf(Error);
		expect(seatErrorOf(err)).toEqual({
			code: 'seat_held',
			message: 'lead is held by cli since 1',
			details: { client: 'cli', since: 1, expires_at: 2 },
		});
	});

	it('leaves a plain error a plain Error', async () => {
		answer({ ok: false, error: 'seats_get: `seat` is required' });
		const err = await new WebRemoteTransport().invoke('seats_get', {}).catch((e) => e);
		expect(err).toBeInstanceOf(Error);
		expect((err as Error).message).toBe('seats_get: `seat` is required');
		expect(seatErrorOf(err)).toBeNull();
	});
});
