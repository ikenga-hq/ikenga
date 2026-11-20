import { afterEach, describe, expect, it, vi } from 'vitest';
import { WebRemoteTransport } from './index';

const realFetch = globalThis.fetch;
afterEach(() => {
	globalThis.fetch = realFetch;
});

function reply(status: number, statusText: string, body: () => Promise<unknown>) {
	globalThis.fetch = vi.fn().mockResolvedValue({ ok: false, status, statusText, json: body });
}

describe('WebRemoteTransport non-200 replies', () => {
	it('rejects with the refusal sentence from a JSON body', async () => {
		reply(403, 'Forbidden', async () => ({ ok: false, error: 'expired: this share has ended' }));
		await expect(new WebRemoteTransport().invoke('fs_read')).rejects.toThrow(
			'expired: this share has ended'
		);
	});

	it('copies typed error_data onto the error', async () => {
		reply(403, 'Forbidden', async () => ({
			ok: false,
			error: 'refused',
			error_data: { code: 'share_expired' },
		}));
		await expect(new WebRemoteTransport().invoke('fs_read')).rejects.toMatchObject({
			message: 'refused',
			code: 'share_expired',
		});
	});

	it('falls back to the status line when the body is not JSON', async () => {
		reply(502, 'Bad Gateway', async () => {
			throw new SyntaxError('Unexpected token <');
		});
		await expect(new WebRemoteTransport().invoke('fs_read')).rejects.toThrow(
			'HTTP RPC error: 502 Bad Gateway'
		);
	});

	it('falls back when the JSON has no error string', async () => {
		reply(500, 'Internal Server Error', async () => ({ ok: false }));
		await expect(new WebRemoteTransport().invoke('fs_read')).rejects.toThrow(
			'HTTP RPC error: 500 Internal Server Error'
		);
	});
});
