// WP-P9 — the browser client for in-app server updates: who may ask, what a
// refusal looks like (silent `null`, never a re-auth prompt), and the typed
// apply result.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	applyServerUpdate,
	fetchServerUpdate,
	isServerUpdateInFlight,
	ServerUnreachableError,
} from './server-update';
import { serverUpdateRun, serverUpdateView } from './server-update.fixtures';
import { __setT1SessionForTests } from './t1-session';

// A T0 tab that opened with the operator link (read once, on first use).
sessionStorage.setItem('ikenga_auth_token', 'op-token');

const view = serverUpdateView;

function json(body: unknown, status = 200): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json' },
	});
}

const fetchMock = vi.fn<typeof fetch>();

beforeEach(() => {
	__setT1SessionForTests(false);
	fetchMock.mockReset();
	vi.stubGlobal('fetch', fetchMock);
});

afterEach(() => {
	vi.unstubAllGlobals();
	__setT1SessionForTests(false);
	delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
});

describe('fetchServerUpdate', () => {
	it('never asks from the desktop app', async () => {
		(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
		expect(await fetchServerUpdate()).toBeNull();
		expect(fetchMock).not.toHaveBeenCalled();
	});

	it('never asks for a T1 member, and asks for a T1 admin with the cookie only', async () => {
		__setT1SessionForTests(true, { principal_id: 'p', username: 'bob', is_admin: false });
		expect(await fetchServerUpdate()).toBeNull();
		expect(fetchMock).not.toHaveBeenCalled();

		__setT1SessionForTests(true, { principal_id: 'p', username: 'ada', is_admin: true });
		fetchMock.mockResolvedValueOnce(json({ ok: true, data: view() }));
		expect((await fetchServerUpdate())?.available?.version).toBe('0.21.0');
		const [url, init] = fetchMock.mock.calls[0];
		expect(url).toBe('/api/server/update');
		expect(init?.credentials).toBe('same-origin');
		expect((init?.headers as Record<string, string>).Authorization).toBeUndefined();
	});

	it('sends the T0 operator bearer', async () => {
		fetchMock.mockResolvedValueOnce(json({ ok: true, data: view() }));
		await fetchServerUpdate();
		const init = fetchMock.mock.calls[0][1];
		expect((init?.headers as Record<string, string>).Authorization).toBe('Bearer op-token');
	});

	it('is silent (null) on 401, 403, 404 and supported:false', async () => {
		for (const res of [
			json({ ok: false, code: 'unauthorized' }, 401),
			json({ ok: false, code: 'forbidden' }, 403),
			json({ ok: false, code: 'unsupported' }, 404),
			json({ ok: true, data: view({ supported: false, available: null }) }),
		]) {
			fetchMock.mockResolvedValueOnce(res);
			expect(await fetchServerUpdate()).toBeNull();
		}
	});

	it('throws on a network failure or a 5xx, so a poll mid-restart keeps the last view', async () => {
		fetchMock.mockRejectedValueOnce(new TypeError('connection refused'));
		await expect(fetchServerUpdate()).rejects.toBeInstanceOf(ServerUnreachableError);
		fetchMock.mockResolvedValueOnce(new Response('bad gateway', { status: 502 }));
		await expect(fetchServerUpdate()).rejects.toBeInstanceOf(ServerUnreachableError);
	});
});

describe('applyServerUpdate', () => {
	it('posts the version and the acknowledged count', async () => {
		fetchMock.mockResolvedValueOnce(
			json({ ok: true, data: { state: 'queued', version: '0.21.0', request_id: 'r-1' } }, 202)
		);
		const r = await applyServerUpdate({ version: '0.21.0', acknowledgedOpenTerminals: 2 });
		expect(r).toEqual({ ok: true, version: '0.21.0', requestId: 'r-1' });
		const [url, init] = fetchMock.mock.calls[0];
		expect(url).toBe('/api/server/update/apply');
		expect(init?.method).toBe('POST');
		expect(JSON.parse(String(init?.body))).toEqual({
			version: '0.21.0',
			acknowledged_open_terminals: 2,
		});
	});

	it('types a 409 terminals_open with the count that is true now', async () => {
		fetchMock.mockResolvedValueOnce(
			json({ ok: false, code: 'terminals_open', error: 'ends 5', open_terminals: 5 }, 409)
		);
		const r = await applyServerUpdate({ version: '0.21.0', acknowledgedOpenTerminals: 2 });
		expect(r).toEqual({ ok: false, code: 'terminals_open', message: 'ends 5', openTerminals: 5 });
	});

	it('refuses locally for a T1 member', async () => {
		__setT1SessionForTests(true, { principal_id: 'p', username: 'bob', is_admin: false });
		const r = await applyServerUpdate({ version: '0.21.0', acknowledgedOpenTerminals: 0 });
		expect(r.ok).toBe(false);
		expect(fetchMock).not.toHaveBeenCalled();
	});
});

describe('isServerUpdateInFlight', () => {
	it('is true while root runs or a request waits', () => {
		expect(isServerUpdateInFlight(view())).toBe(false);
		expect(
			isServerUpdateInFlight(
				view({
					pending_request: {
						version: '0.21.0',
						request_id: 'r',
						requested_by: 'ada',
						requested_at: null,
					},
				})
			)
		).toBe(true);
		expect(
			isServerUpdateInFlight(
				view({
					last_run: serverUpdateRun({ state: 'running', finished_at: null }),
				})
			)
		).toBe(true);
	});
});
