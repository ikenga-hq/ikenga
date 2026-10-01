/**
 * T1 browser sessions (docs/remote/principal-contract.md §2.2–§2.4, OD-12).
 *
 * On a T1 (multi-user) server the browser holds no bearer token. It signs in
 * with a username and password at `POST /auth/login`, and the broker answers
 * with an HttpOnly `ikenga_session` cookie. Same-origin `fetch` and
 * `new WebSocket` both send that cookie, so under T1 the transport never
 * appends `?token=`. The broker would ignore one anyway (I-6).
 *
 * The tier is read once at boot from the unauthenticated `/api/health`
 * (`executor.tier`), and only for a browser tab with no token (see
 * `boot/primary.tsx`). A desktop window (Tauri) never asks, and a
 * T0 tab opened from its `?token=` link never asks. Both keep exactly the
 * behaviour they had before.
 *
 * This module has no imports, so the boot path can load it before the
 * transport picks a backend.
 */

/** `GET /auth/me`. The display name comes from here, not from `os_username`
 *  (§5 row 7). */
export interface AuthMe {
	principal_id: string;
	username: string;
	is_admin: boolean;
}

export type SignInResult =
	| { ok: true }
	| {
			ok: false;
			reason: 'invalid' | 'throttled' | 'error';
			message: string;
			retryAfterSecs?: number;
	  };

let t1 = false;
let me: AuthMe | null = null;

/** True once {@link detectT1Server} saw a T1 server. False on desktop, on T0,
 *  and before detection has run. */
export function isT1Session(): boolean {
	return t1;
}

/** The signed-in principal, once {@link fetchAuthMe} has resolved one. */
export function currentPrincipal(): AuthMe | null {
	return me;
}

/**
 * Read `/api/health` and record whether this server is T1. Never throws:
 * any failure (network, non-JSON, a server too old to report a tier) leaves
 * the session in its pre-T1 behaviour.
 */
export async function detectT1Server(): Promise<boolean> {
	try {
		const res = await fetch('/api/health', { credentials: 'same-origin' });
		if (!res.ok) return false;
		const body = (await res.json()) as { executor?: { tier?: unknown } } | null;
		t1 = body?.executor?.tier === 't1';
	} catch {
		t1 = false;
	}
	return t1;
}

/** `GET /auth/me` with the session cookie. `null` when not signed in. */
export async function fetchAuthMe(): Promise<AuthMe | null> {
	try {
		const res = await fetch('/auth/me', { credentials: 'same-origin' });
		if (!res.ok) {
			me = null;
			return null;
		}
		const body = (await res.json()) as Partial<AuthMe> | null;
		me =
			body && typeof body.username === 'string' && typeof body.principal_id === 'string'
				? {
						principal_id: body.principal_id,
						username: body.username,
						is_admin: body.is_admin === true,
					}
				: null;
	} catch {
		me = null;
	}
	return me;
}

/**
 * `POST /auth/login {username, password}`. A `204` means the broker has set
 * the session cookie. The error wording doesn't say which part was wrong,
 * because the server doesn't say either.
 */
export async function signInWithPassword(
	username: string,
	password: string
): Promise<SignInResult> {
	let res: Response;
	try {
		res = await fetch('/auth/login', {
			method: 'POST',
			credentials: 'same-origin',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ username, password }),
		});
	} catch (err) {
		return { ok: false, reason: 'error', message: `Connection failed: ${String(err)}` };
	}
	if (res.ok) return { ok: true };
	if (res.status === 401) {
		return { ok: false, reason: 'invalid', message: 'Wrong username or password.' };
	}
	if (res.status === 429) {
		const secs = Number(res.headers.get('Retry-After'));
		const retryAfterSecs = Number.isFinite(secs) && secs > 0 ? secs : undefined;
		return {
			ok: false,
			reason: 'throttled',
			message: retryAfterSecs
				? `Too many attempts. Try again in ${retryAfterSecs} s.`
				: 'Too many attempts. Try again later.',
			retryAfterSecs,
		};
	}
	return { ok: false, reason: 'error', message: `Sign-in failed (HTTP ${res.status}).` };
}

/** Test-only: set the detected state directly. */
export function __setT1SessionForTests(on: boolean, who: AuthMe | null = null): void {
	t1 = on;
	me = who;
}
