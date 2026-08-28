// Remote-access token handling: the token arrives in the URL and must not
// stay there, or in any storage that outlives the tab.

import { beforeEach, describe, expect, it, vi } from 'vitest';

const TOKEN_KEY = 'ikenga_auth_token';

async function freshModule() {
	vi.resetModules();
	return import('./index');
}

function visit(search: string) {
	window.history.replaceState(null, '', `/app${search}`);
}

describe('getAuthToken', () => {
	beforeEach(() => {
		sessionStorage.clear();
		localStorage.clear();
		visit('');
	});

	it('reads the token from the URL', async () => {
		visit('?token=abc123');
		const { getAuthToken } = await freshModule();
		expect(getAuthToken()).toBe('abc123');
	});

	it('strips the token from the URL so it stays out of history and Referer', async () => {
		visit('?token=abc123&pane=terminal');
		const { getAuthToken } = await freshModule();
		getAuthToken();
		expect(window.location.search).not.toContain('abc123');
		expect(window.location.search).not.toContain('token');
		// Unrelated params survive.
		expect(window.location.search).toContain('pane=terminal');
	});

	it('backs the token with sessionStorage, never localStorage', async () => {
		visit('?token=abc123');
		const { getAuthToken } = await freshModule();
		getAuthToken();
		expect(sessionStorage.getItem(TOKEN_KEY)).toBe('abc123');
		expect(localStorage.getItem(TOKEN_KEY)).toBeNull();
	});

	it('survives a reload once the URL no longer carries it', async () => {
		sessionStorage.setItem(TOKEN_KEY, 'from-session');
		const { getAuthToken } = await freshModule();
		expect(getAuthToken()).toBe('from-session');
	});

	it('clears a token left in localStorage by an earlier build', async () => {
		localStorage.setItem(TOKEN_KEY, 'stale-and-persistent');
		const { getAuthToken } = await freshModule();
		expect(getAuthToken()).toBeNull();
		expect(localStorage.getItem(TOKEN_KEY)).toBeNull();
	});

	it('returns null when no token is anywhere', async () => {
		const { getAuthToken } = await freshModule();
		expect(getAuthToken()).toBeNull();
	});

	it('clearAuthToken drops it from memory and storage', async () => {
		visit('?token=abc123');
		const { getAuthToken, clearAuthToken } = await freshModule();
		expect(getAuthToken()).toBe('abc123');
		clearAuthToken();
		expect(getAuthToken()).toBeNull();
		expect(sessionStorage.getItem(TOKEN_KEY)).toBeNull();
	});
});

describe('WebRemoteTransport.listen', () => {
	it('warns once per event name that nothing will fire', async () => {
		const { WebRemoteTransport } = await freshModule();
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		const t = new WebRemoteTransport();

		await t.listen('fs://1', () => {});
		await t.listen('fs://1', () => {});
		await t.listen('projects:active-changed', () => {});

		expect(warn).toHaveBeenCalledTimes(2);
		expect(warn.mock.calls[0]?.[0]).toContain("listen('fs://1')");
		warn.mockRestore();
	});

	it('fans out through dispatch once a producer exists', async () => {
		const { WebRemoteTransport } = await freshModule();
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		const t = new WebRemoteTransport();

		const seen: unknown[] = [];
		const off = await t.listen<{ id: string }>('projects:active-changed', (e) =>
			seen.push(e.payload)
		);

		t.dispatch('projects:active-changed', { id: 'p1' });
		expect(seen).toEqual([{ id: 'p1' }]);

		off();
		t.dispatch('projects:active-changed', { id: 'p2' });
		expect(seen).toEqual([{ id: 'p1' }]);
	});
});

describe('getTransport', () => {
	beforeEach(() => {
		sessionStorage.clear();
		localStorage.clear();
		visit('');
	});

	it('uses the desktop transport when there is no remote token', async () => {
		// jsdom is not Tauri, but it is not an ikenga-server page either —
		// picking the HTTP transport here turns every mocked `invoke` in the
		// suite into a real fetch('/api/rpc').
		const { getTransport, TauriTransport, isRemoteWebSession } = await freshModule();
		expect(isRemoteWebSession()).toBe(false);
		expect(getTransport()).toBeInstanceOf(TauriTransport);
	});

	it('uses the HTTP transport for a token-bearing browser page', async () => {
		visit('?token=abc123');
		const { getTransport, WebRemoteTransport, isRemoteWebSession } = await freshModule();
		expect(isRemoteWebSession()).toBe(true);
		expect(getTransport()).toBeInstanceOf(WebRemoteTransport);
	});

	it('keeps the HTTP transport across a reload, once the URL is stripped', async () => {
		sessionStorage.setItem(TOKEN_KEY, 'from-session');
		const { getTransport, WebRemoteTransport } = await freshModule();
		expect(getTransport()).toBeInstanceOf(WebRemoteTransport);
	});
});
