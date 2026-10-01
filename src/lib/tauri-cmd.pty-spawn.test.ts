// The desktop returns the bare PTY id from `pty_spawn`; the headless daemon's
// /api/rpc arm returns `{ pty_id }`. A browser session that passed the object on
// built `/ws/pty/[object Object]` and the terminal never attached.

import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('./transport', async (importOriginal) => {
	const actual = await importOriginal<typeof import('./transport')>();
	// tauri-cmd's `invoke` is `getTransport().invoke`, so the transport is what to stub.
	return { ...actual, getTransport: () => ({ invoke: (...a: unknown[]) => invokeMock(...a) }) };
});

import { ptySpawn } from './tauri-cmd';

const opts = { cwd: '/home/rex', cmd: ['/bin/bash'] };

describe('ptySpawn id shape', () => {
	beforeEach(() => invokeMock.mockReset());

	it('returns the id unchanged when the backend returns a bare string (desktop)', async () => {
		invokeMock.mockResolvedValue('pty-abc');
		await expect(ptySpawn(opts)).resolves.toBe('pty-abc');
	});

	it('unwraps { pty_id } from the headless daemon', async () => {
		invokeMock.mockResolvedValue({ pty_id: 'pty-xyz' });
		const id = await ptySpawn(opts);
		expect(id).toBe('pty-xyz');
		expect(typeof id).toBe('string');
		expect(String(id)).not.toContain('[object');
	});
});
