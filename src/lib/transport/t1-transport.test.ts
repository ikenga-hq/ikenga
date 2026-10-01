// Under T1 the transport is cookie-only (G-PRINCIPAL §2.3, I-6): no bearer
// header and no `?token=` on any socket, even with a stale T0 token in this
// tab's storage. Under T0 nothing changes.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const TOKEN_KEY = 'ikenga_auth_token';

/** Fresh copies of the transport and the T1 flag it reads (one registry). */
async function fresh() {
	vi.resetModules();
	const t1 = await import('./t1-session');
	const transport = await import('./index');
	return { t1, transport };
}

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

beforeEach(() => {
	sessionStorage.clear();
	localStorage.clear();
	window.history.replaceState(null, '', '/app');
	FakeWebSocket.urls = [];
	fetchMock.mockReset();
	fetchMock.mockResolvedValue(
		new Response(JSON.stringify({ ok: true, data: [] }), { status: 200 })
	);
	vi.stubGlobal('fetch', fetchMock);
	vi.stubGlobal('WebSocket', FakeWebSocket);
});

afterEach(() => {
	vi.unstubAllGlobals();
});

describe('T1 transport', () => {
	it('is a remote web session with no token, on the HTTP transport', async () => {
		const { t1, transport } = await fresh();
		expect(transport.isRemoteWebSession()).toBe(false);
		t1.__setT1SessionForTests(true);
		expect(transport.isRemoteWebSession()).toBe(true);
		expect(transport.getTransport()).toBeInstanceOf(transport.WebRemoteTransport);
	});

	it('replaces a desktop transport picked before the tier was known', async () => {
		const { t1, transport } = await fresh();
		expect(transport.getTransport()).toBeInstanceOf(transport.TauriTransport);
		t1.__setT1SessionForTests(true);
		expect(transport.getTransport()).toBeInstanceOf(transport.WebRemoteTransport);
	});

	it('sends RPCs with the cookie only, never a stale T0 token', async () => {
		sessionStorage.setItem(TOKEN_KEY, 'stale-t0-token');
		const { t1, transport } = await fresh();
		t1.__setT1SessionForTests(true);
		expect(transport.transportToken()).toBeNull();
		await new transport.WebRemoteTransport().invoke('fs_roots_list');
		const [url, init] = fetchMock.mock.calls[0]!;
		expect(url).toBe('/api/rpc');
		expect(init?.credentials).toBe('same-origin');
		expect(init?.headers).not.toHaveProperty('Authorization');
		expect(JSON.stringify(init)).not.toContain('stale-t0-token');
	});

	it('opens the PTY, fs and chat sockets without ?token=', async () => {
		sessionStorage.setItem(TOKEN_KEY, 'stale-t0-token');
		const { t1, transport } = await fresh();
		t1.__setT1SessionForTests(true);
		const web = new transport.WebRemoteTransport();
		web.openPtySocket('term 1', { spawn: true });
		web.openFsSocket();
		const { ChatWebSocketClient } = await import('./chat-client');
		new ChatWebSocketClient('thread-1', () => {}).connect();
		expect(FakeWebSocket.urls).toEqual([
			`ws://${window.location.host}/ws/pty/term%201?spawn=true`,
			`ws://${window.location.host}/ws/fs`,
			`ws://${window.location.host}/ws/chat/thread-1`,
		]);
	});

	it('opens the sign-in dialog on a 401', async () => {
		const { t1, transport } = await fresh();
		t1.__setT1SessionForTests(true);
		fetchMock.mockResolvedValueOnce(new Response('{}', { status: 401 }));
		await expect(new transport.WebRemoteTransport().invoke('fs_roots_list')).rejects.toThrow('401');
		const { useReauthStore } = await import('./reauth-store');
		expect(useReauthStore.getState().isOpen).toBe(true);
	});
});

describe('T0 transport is unchanged', () => {
	it('keeps the bearer header and ?token=, and adds no credentials option', async () => {
		window.history.replaceState(null, '', '/app?token=t0-token');
		const { transport } = await fresh();
		expect(transport.transportToken()).toBe('t0-token');
		const web = new transport.WebRemoteTransport();
		await web.invoke('fs_roots_list');
		const [, init] = fetchMock.mock.calls[0]!;
		expect(init).not.toHaveProperty('credentials');
		expect(init?.headers).toMatchObject({ Authorization: 'Bearer t0-token' });
		web.openPtySocket('t');
		web.openFsSocket();
		const { ChatWebSocketClient } = await import('./chat-client');
		new ChatWebSocketClient('th', () => {}).connect();
		expect(FakeWebSocket.urls).toEqual([
			`ws://${window.location.host}/ws/pty/t?token=t0-token`,
			`ws://${window.location.host}/ws/fs?token=t0-token`,
			`ws://${window.location.host}/ws/chat/th?token=t0-token`,
		]);
	});
});
