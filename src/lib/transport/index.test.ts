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
	/** A WebSocket stand-in that records what was opened and sent. */
	function fakeSockets() {
		const opened: Array<{ sent: string[]; open(): void; frame(v: unknown): void }> = [];
		const open = () => {
			const s = {
				onopen: null as null | (() => void),
				onmessage: null as null | ((e: { data: unknown }) => void),
				onclose: null as null | (() => void),
				onerror: null as null | ((e: unknown) => void),
				sent: [] as string[],
				send(raw: string) {
					this.sent.push(raw);
				},
				close() {},
				open() {
					this.onopen?.();
				},
				frame(v: unknown) {
					this.onmessage?.({ data: JSON.stringify(v) });
				},
			};
			opened.push(s);
			return s as unknown as WebSocket;
		};
		return { opened, open };
	}

	it('subscribes over the events socket and delivers its frames', async () => {
		const { WebRemoteTransport } = await freshModule();
		const { opened, open } = fakeSockets();
		const t = new WebRemoteTransport({ openEventsSocket: open });

		const seen: unknown[] = [];
		const off = await t.listen<{ id: string }>('projects:active-changed', (e) =>
			seen.push(e.payload)
		);
		expect(opened).toHaveLength(1);
		opened[0].open();
		expect(opened[0].sent.map((s) => JSON.parse(s))).toEqual([
			{ type: 'subscribe', events: ['projects:active-changed'] },
		]);

		opened[0].frame({ type: 'event', event: 'projects:active-changed', payload: { id: 'p1' } });
		expect(seen).toEqual([{ id: 'p1' }]);

		off();
		opened[0].frame({ type: 'event', event: 'projects:active-changed', payload: { id: 'p2' } });
		expect(seen).toEqual([{ id: 'p1' }]);
	});

	it('no longer warns that a subscription will never fire', async () => {
		const { WebRemoteTransport } = await freshModule();
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		const t = new WebRemoteTransport({ openEventsSocket: fakeSockets().open });
		await t.listen('settings://changed', () => {});
		expect(warn).not.toHaveBeenCalled();
		warn.mockRestore();
	});

	it('fans out through dispatch to local listeners', async () => {
		const { WebRemoteTransport } = await freshModule();
		const t = new WebRemoteTransport({ openEventsSocket: fakeSockets().open });
		const seen: unknown[] = [];
		await t.listen('pa-action-paused', (e) => seen.push(e.payload));
		t.dispatch('pa-action-paused', { batchId: 'b', count: 1 });
		expect(seen).toEqual([{ batchId: 'b', count: 1 }]);
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

	it('switches to the HTTP transport once a paired device is detected (WP-78b)', async () => {
		// Something can call `invoke` before the boot probe answers (the log
		// bridge flushing a console line); the desktop transport it caches
		// must not outlive the device-cookie detection.
		const { getTransport, TauriTransport, WebRemoteTransport } = await freshModule();
		expect(getTransport()).toBeInstanceOf(TauriTransport);
		const { detectAccessStatus } = await import('./device-session');
		const fetchMock = vi.fn(
			async () =>
				new Response(
					JSON.stringify({
						ok: true,
						data: {
							tier: 't0',
							credential: { via: 'device', deviceId: 'd1', tier: 'dispatch' },
							caps: ['files', 'sessions', 'dispatch'],
							adminStrength: false,
						},
					})
				)
		);
		vi.stubGlobal('fetch', fetchMock);
		try {
			await detectAccessStatus();
		} finally {
			vi.unstubAllGlobals();
		}
		expect(getTransport()).toBeInstanceOf(WebRemoteTransport);
	});

	it('keeps the HTTP transport across a reload, once the URL is stripped', async () => {
		sessionStorage.setItem(TOKEN_KEY, 'from-session');
		const { getTransport, WebRemoteTransport } = await freshModule();
		expect(getTransport()).toBeInstanceOf(WebRemoteTransport);
	});
});

describe('awaitingFirstToken', () => {
	beforeEach(() => {
		sessionStorage.clear();
		localStorage.clear();
		visit('');
		delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
	});

	it('is true for a browser tab with no token: the connect dialog must show, not a blank page', async () => {
		const { awaitingFirstToken } = await freshModule();
		expect(awaitingFirstToken()).toBe(true);
	});

	it('is false once the tab holds a token, from the URL or from sessionStorage', async () => {
		visit('?token=abc123');
		const first = await freshModule();
		expect(first.awaitingFirstToken()).toBe(false);

		visit('');
		sessionStorage.setItem(TOKEN_KEY, 'from-session');
		const second = await freshModule();
		expect(second.awaitingFirstToken()).toBe(false);
	});

	it('is false for a T1 cookie session, which has no token by design', async () => {
		const { awaitingFirstToken } = await freshModule();
		// Same registry as `./index` just imported (no reset in between).
		const { __setT1SessionForTests } = await import('./t1-session');
		__setT1SessionForTests(true, null);
		try {
			expect(awaitingFirstToken()).toBe(false);
		} finally {
			__setT1SessionForTests(false, null);
		}
	});

	it('is never true under Tauri, where there is no token to wait for', async () => {
		(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
		const { awaitingFirstToken } = await freshModule();
		expect(awaitingFirstToken()).toBe(false);
		delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
	});

	it('does not change which transport a token-less page gets', async () => {
		const { getTransport, TauriTransport, isRemoteWebSession } = await freshModule();
		expect(isRemoteWebSession()).toBe(false);
		expect(getTransport()).toBeInstanceOf(TauriTransport);
	});
});
