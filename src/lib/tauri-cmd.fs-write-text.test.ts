// plans/file-editing Shape 5: a browser text save sends the daemon's compact
// `content` string arm (a JSON number array held saves to ~400 KB under the
// daemon's old 2 MB body default); the desktop command takes `bytes` only.
// An oversized remote save is refused before sending, and a 413 is mapped to
// a clear message — loud, never a truncated file.

import { beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ invoke: vi.fn(), remote: false }));
vi.mock('./transport', async (importOriginal) => {
	const actual = await importOriginal<typeof import('./transport')>();
	return {
		...actual,
		isRemoteWebSession: () => h.remote,
		getTransport: () => ({ invoke: (...a: unknown[]) => h.invoke(...a) }),
	};
});

import { fsWriteText, RPC_BODY_LIMIT_BYTES } from './tauri-cmd';

beforeEach(() => {
	h.invoke.mockReset();
	h.invoke.mockResolvedValue(undefined);
});

describe('fsWriteText', () => {
	it('desktop: sends UTF-8 bytes (the Tauri command takes bytes only)', async () => {
		h.remote = false;
		await fsWriteText('/w/a.txt', 'hé');
		expect(h.invoke).toHaveBeenCalledWith('fs_write', { path: '/w/a.txt', bytes: [104, 195, 169] });
	});

	it('browser: sends the text as `content`', async () => {
		h.remote = true;
		await fsWriteText('/w/a.txt', 'hé');
		expect(h.invoke).toHaveBeenCalledWith('fs_write', { path: '/w/a.txt', content: 'hé' });
	});

	it('browser: refuses a body over the daemon limit without sending', async () => {
		h.remote = true;
		await expect(fsWriteText('/w/big.txt', 'x'.repeat(RPC_BODY_LIMIT_BYTES))).rejects.toThrow(
			/too large to save over the remote connection/
		);
		expect(h.invoke).not.toHaveBeenCalled();
	});

	it('browser: maps a 413 to the same clear message', async () => {
		h.remote = true;
		h.invoke.mockRejectedValue(new Error('HTTP RPC error: 413 Payload Too Large'));
		await expect(fsWriteText('/w/a.txt', 'x')).rejects.toThrow(/Nothing was written/);
	});

	it('browser: passes other errors through unchanged', async () => {
		h.remote = true;
		h.invoke.mockRejectedValue(new Error('outside allowlist'));
		await expect(fsWriteText('/w/a.txt', 'x')).rejects.toThrow('outside allowlist');
	});
});
