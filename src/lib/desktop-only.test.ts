import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false }));
vi.mock('@/lib/tauri-cmd', () => ({ isRemoteWebSession: () => h.remote }));

import { installUnavailableReason, NOT_AVAILABLE_ON_SERVER } from './desktop-only';
import { honestRpcError } from './transport/unavailable';

afterEach(() => {
	h.remote = false;
});

describe('installUnavailableReason (gap rank 3)', () => {
	it('is the honest sentence in a remote session', () => {
		h.remote = true;
		expect(installUnavailableReason()).toBe('Not available on this server yet');
		expect(NOT_AVAILABLE_ON_SERVER).toBe('Not available on this server yet');
	});
	it('is false on the desktop', () => {
		expect(installUnavailableReason()).toBe(false);
	});
});

describe('honestRpcError', () => {
	it('keeps a genuine failure that merely contains a loose phrase', () => {
		for (const m of [
			'npm ERR! engine node@16 is not supported by this package',
			'pkg manifest: unknown command "foo" in run block',
		])
			expect(honestRpcError(new Error(m))).toBe(m);
	});
	it('never lets a raw "not implemented" string through', () => {
		expect(
			honestRpcError(
				new Error("Command 'pkg_install_from_path' not implemented in headless daemon")
			)
		).toBe('Not available on this server yet');
	});
	it('keeps a real failure as itself', () => {
		expect(honestRpcError(new Error('EACCES: permission denied'))).toBe(
			'EACCES: permission denied'
		);
		expect(honestRpcError('boom')).toBe('boom');
	});
});
