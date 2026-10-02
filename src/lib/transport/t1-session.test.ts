// T1 sign-in (G-PRINCIPAL §2.2–§2.4): tier detection from /api/health, the
// /auth/me identity, and POST /auth/login with the session cookie.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	__setT1SessionForTests,
	currentPrincipal,
	detectT1Server,
	fetchAuthMe,
	isT1Session,
	signInWithPassword,
} from './t1-session';

function json(body: unknown, status = 200, headers: Record<string, string> = {}): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'Content-Type': 'application/json', ...headers },
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
});

describe('detectT1Server', () => {
	it('is T1 only when /api/health reports executor.tier "t1"', async () => {
		fetchMock.mockResolvedValueOnce(json({ ok: true, executor: { tier: 't1' } }));
		expect(await detectT1Server()).toBe(true);
		expect(isT1Session()).toBe(true);
		expect(fetchMock).toHaveBeenCalledWith('/api/health', { credentials: 'same-origin' });

		fetchMock.mockResolvedValueOnce(json({ ok: true, executor: { tier: 't0' } }));
		expect(await detectT1Server()).toBe(false);
		expect(isT1Session()).toBe(false);
	});

	it('never throws, and anything unexpected keeps the pre-T1 behaviour', async () => {
		fetchMock.mockRejectedValueOnce(new TypeError('network down'));
		expect(await detectT1Server()).toBe(false);
		fetchMock.mockResolvedValueOnce(new Response('<html>', { status: 200 }));
		expect(await detectT1Server()).toBe(false);
		fetchMock.mockResolvedValueOnce(json({ ok: true }));
		expect(await detectT1Server()).toBe(false);
		fetchMock.mockResolvedValueOnce(json({}, 503));
		expect(await detectT1Server()).toBe(false);
		expect(isT1Session()).toBe(false);
	});
});

describe('fetchAuthMe', () => {
	it('returns and remembers the signed-in principal', async () => {
		fetchMock.mockResolvedValueOnce(
			json({
				principal_id: '01890a5d-ac96-774b-bcce-b302099a8057',
				username: 'ada',
				is_admin: true,
			})
		);
		const me = await fetchAuthMe();
		expect(me).toEqual({
			principal_id: '01890a5d-ac96-774b-bcce-b302099a8057',
			username: 'ada',
			is_admin: true,
		});
		expect(currentPrincipal()).toEqual(me);
		expect(fetchMock).toHaveBeenCalledWith('/auth/me', { credentials: 'same-origin' });
	});

	it('is null when signed out, and forgets a previous principal', async () => {
		__setT1SessionForTests(true, { principal_id: 'x', username: 'old', is_admin: false });
		fetchMock.mockResolvedValueOnce(json({ ok: false, code: 'unauthenticated' }, 401));
		expect(await fetchAuthMe()).toBeNull();
		expect(currentPrincipal()).toBeNull();
	});
});

describe('signInWithPassword', () => {
	it('POSTs the credentials same-origin and succeeds on 204', async () => {
		fetchMock.mockResolvedValueOnce(new Response(null, { status: 204 }));
		expect(await signInWithPassword('ada', 'correct horse')).toEqual({ ok: true });
		const [url, init] = fetchMock.mock.calls[0]!;
		expect(url).toBe('/auth/login');
		expect(init).toMatchObject({ method: 'POST', credentials: 'same-origin' });
		expect(JSON.parse(String(init?.body))).toEqual({ username: 'ada', password: 'correct horse' });
		// No bearer token, ever.
		expect(JSON.stringify(init?.headers)).not.toContain('Authorization');
	});

	it('says only "wrong username or password" on 401', async () => {
		fetchMock.mockResolvedValueOnce(json({ ok: false, code: 'invalid_credentials' }, 401));
		expect(await signInWithPassword('ada', 'nope')).toEqual({
			ok: false,
			reason: 'invalid',
			message: 'Wrong username or password.',
		});
	});

	it('reports the backoff from Retry-After on 429', async () => {
		fetchMock.mockResolvedValueOnce(json({ ok: false }, 429, { 'Retry-After': '8' }));
		const res = await signInWithPassword('ada', 'nope');
		expect(res).toMatchObject({ ok: false, reason: 'throttled', retryAfterSecs: 8 });
		if (!res.ok) expect(res.message).toContain('8 s');
	});

	it('reports other failures without throwing', async () => {
		fetchMock.mockResolvedValueOnce(json({}, 500));
		expect(await signInWithPassword('ada', 'x')).toMatchObject({ ok: false, reason: 'error' });
		fetchMock.mockRejectedValueOnce(new TypeError('offline'));
		expect(await signInWithPassword('ada', 'x')).toMatchObject({ ok: false, reason: 'error' });
	});
});
