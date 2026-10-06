// Audit 2026-10-06 rank 27: on a pre-login T1 page, the `fs_home` and
// `supabase_config_get` probes ran before the tier probe answered. A T1 tab
// has no token by design, so `isRemoteWebSession()` was still false and they
// got the desktop transport, which calls a Tauri `invoke` that does not exist
// in a browser. The SPA entry now marks the page, and the transport is
// chosen by `!isTauri()` from the first call.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const TOKEN_KEY = 'ikenga_auth_token';
const fetchMock = vi.fn<typeof fetch>();

async function fresh() {
	vi.resetModules();
	return import('./index');
}

beforeEach(() => {
	sessionStorage.clear();
	localStorage.clear();
	window.history.replaceState(null, '', '/app');
	delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
	fetchMock.mockReset();
	fetchMock.mockResolvedValue(
		new Response(JSON.stringify({ ok: true, data: '/home/alice' }), { status: 200 })
	);
	vi.stubGlobal('fetch', fetchMock);
});

afterEach(() => {
	vi.unstubAllGlobals();
	delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
});

describe('browser entry', () => {
	it('picks the HTTP transport before the tier is known, with no token', async () => {
		const transport = await fresh();
		await import('./browser-entry');
		expect(transport.getAuthToken()).toBeNull();
		expect(transport.isBrowserEntry()).toBe(true);
		expect(transport.getTransport()).toBeInstanceOf(transport.WebRemoteTransport);
	});

	it('sends the pre-login probes to /api/rpc, never a Tauri invoke', async () => {
		const tauriInvoke = vi.fn();
		vi.doMock('@tauri-apps/api/core', () => ({ invoke: tauriInvoke }));
		try {
			const transport = await fresh();
			await import('./browser-entry');
			const { homeDir } = await import('./path-shim');
			await expect(homeDir()).resolves.toBe('/home/alice');
			expect(fetchMock).toHaveBeenCalledWith('/api/rpc', expect.anything());
			expect(tauriInvoke).not.toHaveBeenCalled();
			expect(transport.getTransport()).toBeInstanceOf(transport.WebRemoteTransport);
		} finally {
			vi.doUnmock('@tauri-apps/api/core');
		}
	});

	it('changes nothing under Tauri', async () => {
		(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
		const transport = await fresh();
		await import('./browser-entry');
		expect(transport.isBrowserEntry()).toBe(false);
		expect(transport.getTransport()).toBeInstanceOf(transport.TauriTransport);
	});

	it('leaves tests and harnesses (no entry) on the desktop transport', async () => {
		const transport = await fresh();
		expect(transport.isBrowserEntry()).toBe(false);
		expect(transport.getTransport()).toBeInstanceOf(transport.TauriTransport);
	});

	it('keeps a T0 token tab on the HTTP transport with its bearer header', async () => {
		sessionStorage.setItem(TOKEN_KEY, 't0-token');
		const transport = await fresh();
		await import('./browser-entry');
		await transport.getTransport().invoke('fs_home');
		const [, init] = fetchMock.mock.calls[0]!;
		expect(init?.headers).toMatchObject({ Authorization: 'Bearer t0-token' });
	});
});

describe('a stray 401 while the sign-in dialog is open', () => {
	it('does not turn a first-visit prompt into "session expired"', async () => {
		const transport = await fresh();
		const { useReauthStore } = await import('./reauth-store');
		useReauthStore.getState().showReauth('first-visit');
		fetchMock.mockResolvedValueOnce(new Response('{}', { status: 401 }));
		await expect(new transport.WebRemoteTransport().invoke('fs_home')).rejects.toThrow('401');
		expect(useReauthStore.getState().isOpen).toBe(true);
		expect(useReauthStore.getState().reason).toBe('first-visit');
	});

	it('keeps the error message the user is reading', async () => {
		const transport = await fresh();
		const { useReauthStore } = await import('./reauth-store');
		useReauthStore.getState().showReauth();
		useReauthStore.getState().setErrorMsg('Wrong username or password.');
		fetchMock.mockResolvedValueOnce(new Response('{}', { status: 401 }));
		await expect(new transport.WebRemoteTransport().invoke('fs_home')).rejects.toThrow('401');
		expect(useReauthStore.getState().errorMsg).toBe('Wrong username or password.');
	});
});
