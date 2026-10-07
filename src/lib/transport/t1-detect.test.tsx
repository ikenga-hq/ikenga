// Review S4-2: tier detection runs for every browser tab, token or not. A
// tab that still holds a T0 token (sessionStorage, or a `?token=` link) and
// meets a T1 server drops it: no bearer header and no `?token=` is sent, and
// the overlay offers sign-in, not a token paste.

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const TOKEN_KEY = 'ikenga_auth_token';

class FakeWebSocket {
	static urls: string[] = [];
	binaryType = '';
	readyState = 0;
	onopen: (() => void) | null = null;
	onclose: (() => void) | null = null;
	onmessage: (() => void) | null = null;
	onerror: (() => void) | null = null;
	constructor(public url: string) {
		FakeWebSocket.urls.push(url);
	}
	close() {}
	send() {}
}

const fetchMock = vi.fn<typeof fetch>();

function health(tier: 't0' | 't1') {
	return new Response(JSON.stringify({ ok: true, executor: { tier } }), { status: 200 });
}

/** Fresh copies of the transport, the T1 flag and the overlay (one registry). */
async function fresh() {
	vi.resetModules();
	const transport = await import('./index');
	const { useReauthStore } = await import('./reauth-store');
	const { ReauthOverlay } = await import('@/components/ui/reauth-overlay');
	return { transport, useReauthStore, ReauthOverlay };
}

beforeEach(() => {
	sessionStorage.clear();
	localStorage.clear();
	window.history.replaceState(null, '', '/app');
	FakeWebSocket.urls = [];
	fetchMock.mockReset();
	vi.stubGlobal('fetch', fetchMock);
	vi.stubGlobal('WebSocket', FakeWebSocket);
});

afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

describe('detectBrowserTier', () => {
	it('drops a stored T0 token on a T1 server: cookie-only transport, sign-in form', async () => {
		sessionStorage.setItem(TOKEN_KEY, 'stale-t0-token');
		const { transport, useReauthStore, ReauthOverlay } = await fresh();
		fetchMock.mockResolvedValueOnce(health('t1'));
		expect(await transport.detectBrowserTier()).toBe(true);
		expect(fetchMock.mock.calls[0]![0]).toBe('/api/health');
		expect(sessionStorage.getItem(TOKEN_KEY)).toBeNull();
		expect(transport.getAuthToken()).toBeNull();

		fetchMock.mockResolvedValueOnce(
			new Response(JSON.stringify({ ok: true, data: [] }), { status: 200 })
		);
		await new transport.WebRemoteTransport().invoke('fs_roots_list');
		const [, init] = fetchMock.mock.calls[1]!;
		expect(init?.headers).not.toHaveProperty('Authorization');
		expect(JSON.stringify(init)).not.toContain('stale-t0-token');
		const web = new transport.WebRemoteTransport();
		web.openPtySocket('t');
		web.openFsSocket();
		web.openEventsSocket();
		expect(FakeWebSocket.urls.some((u) => u.includes('/ws/events'))).toBe(true);
		expect(FakeWebSocket.urls.some((u) => u.includes('token='))).toBe(false);

		useReauthStore.setState({ isOpen: true, errorMsg: null, tokenInput: '' });
		render(<ReauthOverlay />);
		expect(screen.getByLabelText('Username')).toBeTruthy();
		expect(screen.queryByPlaceholderText('Paste auth token...')).toBeNull();
	});

	it('drops a ?token= link on a T1 server and clears it from the address bar', async () => {
		window.history.replaceState(null, '', '/app?token=link-token');
		const { transport } = await fresh();
		fetchMock.mockResolvedValueOnce(health('t1'));
		expect(await transport.detectBrowserTier()).toBe(true);
		expect(window.location.search).toBe('');
		expect(sessionStorage.getItem(TOKEN_KEY)).toBeNull();
		expect(transport.transportToken()).toBeNull();
	});

	it('keeps the token on a T0 server', async () => {
		sessionStorage.setItem(TOKEN_KEY, 't0-token');
		const { transport } = await fresh();
		fetchMock.mockResolvedValueOnce(health('t0'));
		expect(await transport.detectBrowserTier()).toBe(false);
		expect(transport.transportToken()).toBe('t0-token');
	});

	it('never asks from a desktop window', async () => {
		sessionStorage.setItem(TOKEN_KEY, 't0-token');
		vi.stubGlobal('__TAURI_INTERNALS__', {});
		const { transport } = await fresh();
		expect(await transport.detectBrowserTier()).toBe(false);
		expect(fetchMock).not.toHaveBeenCalled();
		expect(sessionStorage.getItem(TOKEN_KEY)).toBe('t0-token');
	});
});
