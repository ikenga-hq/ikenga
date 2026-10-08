// The device side of pairing (G-ACCESS §3.1–§3.8, WP-74b): the code format,
// the SPAKE2 run against `/access/pair/{hello,confirm,status}`, and the poll
// that ends in the `ikenga_device` cookie.
//
// The page that drives it is `remote-pair-page.tsx`. Nothing here holds the
// device credential: the server sets it as an HttpOnly cookie on the first
// `allowed` status (P-10), so page JS never sees it.

import { type Fingerprint, fingerprintPhrase } from '@/lib/access/fingerprint';
import { normalizePairCode } from '@/lib/access/pair-code';
import {
	b64url,
	bytesEqual,
	deriveKeys,
	hostIdentity,
	ID_A,
	pairPassword,
	startA,
	unb64url,
	utf8,
} from '@/lib/access/spake2';

/** The code from a QR link's fragment (`/remote/pair#c=K7P42Q`). */
export function codeFromHash(hash: string): string | null {
	const params = new URLSearchParams(hash.replace(/^#/, ''));
	const c = params.get('c');
	return c ? normalizePairCode(c) : null;
}

/**
 * The host's store id from a QR link's fragment (`#c=…&h=<storeId>`). A
 * scanned code pins idB with it (§3.4), so the binding to the host doesn't
 * rest on the hello reply's `storeId`, which travels on the same untrusted
 * channel (WP-74b review m5). Typed codes have none and fall back to the
 * reply.
 */
export function hostFromHash(hash: string): string | null {
	const h = new URLSearchParams(hash.replace(/^#/, '')).get('h');
	return h && /^[A-Za-z0-9._-]{1,128}$/.test(h) ? h : null;
}

/** §3.6: the FE proposes a name from the UA, e.g. "Pixel 9 · Chrome". */
export function deviceNameFromUA(ua: string): { name: string; platform: string | null } {
	const browser = /Edg\//.test(ua)
		? 'Edge'
		: /Firefox\//.test(ua)
			? 'Firefox'
			: /(Chrome|CriOS)\//.test(ua)
				? 'Chrome'
				: /Safari\//.test(ua)
					? 'Safari'
					: 'Browser';
	const android = /Android [\d.]+; ([^;)]+?)(?: Build|\))/.exec(ua);
	if (android?.[1]) return { name: `${android[1].trim()} · ${browser}`, platform: 'android' };
	if (/iPhone/.test(ua)) return { name: `iPhone · ${browser}`, platform: 'ios' };
	if (/iPad/.test(ua)) return { name: `iPad · ${browser}`, platform: 'ios' };
	if (/Android/.test(ua)) return { name: `Android · ${browser}`, platform: 'android' };
	if (/Mac OS X|Macintosh/.test(ua)) return { name: `Mac · ${browser}`, platform: 'macos' };
	if (/Windows/.test(ua)) return { name: `Windows · ${browser}`, platform: 'windows' };
	if (/CrOS/.test(ua)) return { name: `Chromebook · ${browser}`, platform: 'chromeos' };
	if (/Linux/.test(ua)) return { name: `Linux · ${browser}`, platform: 'linux' };
	return { name: browser, platform: null };
}

/** How a run ended, for the page's outcome states. */
export type PairOutcome =
	| {
			kind: 'allowed';
			deviceId: string;
			tier: string;
			/**
			 * D-16: the cookie probe answered something unexpected (a proxy 502,
			 * a network drop), so whether the browser kept the device cookie is
			 * unconfirmed. Pairing still proceeds, but the page says so. The
			 * value is the reason, e.g. "the check answered HTTP 502".
			 */
			cookieUnconfirmed?: string;
	  }
	/**
	 * The host allowed the device, but the browser didn't keep the
	 * `ikenga_device` cookie — a `Secure` cookie over plain HTTP off
	 * loopback (review M1). The device holds nothing.
	 */
	| { kind: 'cookie_rejected'; deviceId: string }
	/**
	 * The host allowed the device, but its credential check answered 503
	 * `auth_unavailable`, so whether the browser kept the cookie is unknown.
	 * Not a rejection: retrying (opening the workspace) may just work.
	 */
	| { kind: 'auth_unavailable'; deviceId: string }
	| { kind: 'denied' }
	| { kind: 'expired' }
	| { kind: 'burned' }
	| { kind: 'cancelled' }
	| { kind: 'failed' }
	| { kind: 'throttled'; retryAfterMs: number }
	| { kind: 'unreachable' };

export interface PairCallbacks {
	/** The exchange succeeded: show the words and wait for the host. */
	onWords?: (words: Fingerprint) => void;
	/** Abort the poll (the page unmounted). */
	signal?: AbortSignal;
	/** The QR's `h=` store id: pins idB instead of the hello reply's. */
	pinnedStoreId?: string | null;
}

export interface PairDeps {
	fetch?: typeof fetch;
	sleep?: (ms: number) => Promise<void>;
	/** 64 random bytes for the SPAKE2 scalar (tests only). */
	wide?: Uint8Array;
	pollEveryMs?: number;
}

type Json = Record<string, unknown>;

async function call(
	f: typeof fetch,
	path: string,
	init: RequestInit
): Promise<{ status: number; body: Json }> {
	const res = await f(path, { credentials: 'same-origin', cache: 'no-store', ...init });
	let body: Json = {};
	try {
		body = (await res.json()) as Json;
	} catch {
		body = {};
	}
	return { status: res.status, body };
}

function refusal(status: number, body: Json): PairOutcome {
	if (status === 429) {
		return { kind: 'throttled', retryAfterMs: Number(body.retry_after_ms ?? 30_000) };
	}
	return { kind: 'failed' };
}

const STATE_OUTCOME: Record<string, PairOutcome> = {
	denied: { kind: 'denied' },
	expired: { kind: 'expired' },
	burned: { kind: 'burned' },
	cancelled: { kind: 'cancelled' },
};

/**
 * One pairing run: hello → (check the host's confirm) → confirm → words →
 * poll status until the host decides. On `allowed` the response has set the
 * device cookie.
 */
export async function runPairing(
	normalizedCode: string,
	device: { name: string; platform: string | null },
	cb: PairCallbacks = {},
	deps: PairDeps = {}
): Promise<PairOutcome> {
	const f = deps.fetch ?? fetch.bind(globalThis);
	const sleep = deps.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
	const json = (body: Json): RequestInit => ({
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(body),
	});

	const pw = pairPassword(normalizedCode);
	const idA = utf8(ID_A);
	// msgA doesn't depend on idB (only the final key does), so the host's
	// store id can arrive with its reply: the scalar draw is kept, and side A
	// is re-entered with the real idB — same scalar, same msgA.
	const wide = deps.wide ?? crypto.getRandomValues(new Uint8Array(64));
	const a = startA(pw, idA, new Uint8Array(0), wide);

	let hello: { status: number; body: Json };
	try {
		hello = await call(
			f,
			'/access/pair/hello',
			json({
				slot: normalizedCode.slice(0, 1),
				msgA: b64url(a.msg),
				deviceName: device.name,
				platform: device.platform,
			})
		);
	} catch {
		return { kind: 'unreachable' };
	}
	if (hello.status !== 200) return refusal(hello.status, hello.body);
	const pairingId = String(hello.body.pairingId ?? '');
	const storeId = cb.pinnedStoreId ?? String(hello.body.storeId ?? '');
	let msgB: Uint8Array;
	let hostConfirm: Uint8Array;
	try {
		msgB = unb64url(String(hello.body.msgB ?? ''));
		hostConfirm = unb64url(String(hello.body.hostConfirm ?? ''));
	} catch {
		return { kind: 'failed' };
	}

	let key: Uint8Array;
	try {
		key = startA(pw, idA, hostIdentity(storeId), wide).finish(msgB);
	} catch {
		return { kind: 'failed' };
	}
	const keys = deriveKeys(key, pairingId, a.msg, msgB);

	// A wrong code (or a host that isn't who it says): our device confirm is
	// still sent — it fails and burns the code, so the host sees it.
	const hostOk = bytesEqual(keys.hostConfirm, hostConfirm);
	let confirm: { status: number; body: Json };
	try {
		confirm = await call(
			f,
			'/access/pair/confirm',
			json({ pairingId, deviceConfirm: b64url(keys.deviceConfirm) })
		);
	} catch {
		return { kind: 'unreachable' };
	}
	if (!hostOk) return { kind: 'failed' };
	if (confirm.status !== 200) return refusal(confirm.status, confirm.body);

	cb.onWords?.(fingerprintPhrase(keys.fpSeed));

	const poll = b64url(keys.pollKey);
	for (;;) {
		if (cb.signal?.aborted) return { kind: 'cancelled' };
		let st: { status: number; body: Json };
		try {
			st = await call(f, `/access/pair/status?id=${encodeURIComponent(pairingId)}`, {
				method: 'GET',
				headers: { 'X-Ikenga-Pair-Poll': poll },
			});
		} catch {
			await sleep(deps.pollEveryMs ?? 1500);
			continue;
		}
		if (st.status === 410) return { kind: 'failed' };
		if (st.status !== 200) return refusal(st.status, st.body);
		const state = String(st.body.state ?? '');
		if (state === 'allowed') {
			const deviceId = String(st.body.device_id ?? '');
			// The status response set the cookie — if the browser kept it.
			const cookie = await probeDeviceCookieDetailed(f, deviceId);
			if (cookie.state === 'missing') return { kind: 'cookie_rejected', deviceId };
			if (cookie.state === 'auth_unavailable') return { kind: 'auth_unavailable', deviceId };
			const tier = String(st.body.tier ?? '');
			if (cookie.state === 'unknown') {
				return { kind: 'allowed', deviceId, tier, cookieUnconfirmed: cookie.detail };
			}
			return { kind: 'allowed', deviceId, tier };
		}
		const done = STATE_OUTCOME[state];
		if (done) return done;
		await sleep(deps.pollEveryMs ?? 1500);
	}
}

/**
 * After `allowed`: did the browser keep the device cookie? `access_status`
 * over `/api/rpc` with the cookie only (review M1). `missing` only on
 * positive evidence: a 401, or a 200 that names another (or no) credential.
 * A 503 `auth_unavailable` means the server couldn't check credentials at
 * all; any other failure (network, 5xx) is `unknown` — the boot path finds
 * out then. Neither is reported as a dropped cookie.
 */
export async function probeDeviceCookie(
	f: typeof fetch,
	deviceId: string
): Promise<CookieProbeState> {
	return (await probeDeviceCookieDetailed(f, deviceId)).state;
}

type CookieProbeState = 'ok' | 'missing' | 'auth_unavailable' | 'unknown';

/** `probeDeviceCookie` plus, for `unknown`, why (for the D-16 warning). */
export async function probeDeviceCookieDetailed(
	f: typeof fetch,
	deviceId: string
): Promise<{ state: CookieProbeState; detail?: string }> {
	let res: { status: number; body: Json };
	try {
		res = await call(f, '/api/rpc', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ cmd: 'access_status', args: {} }),
		});
	} catch {
		return { state: 'unknown', detail: "the check didn't reach the computer" };
	}
	const data = res.body.data as
		| { credential?: { via?: string; deviceId?: string | null } }
		| undefined;
	if (res.status === 503 && res.body.code === 'auth_unavailable') return { state: 'auth_unavailable' };
	if (res.status === 401) return { state: 'missing' };
	if (res.status !== 200) return { state: 'unknown', detail: `the check answered HTTP ${res.status}` };
	if (!res.body.ok) return { state: 'unknown', detail: 'the check returned an error' };
	const cred = data?.credential;
	return { state: cred?.via === 'device' && cred.deviceId === deviceId ? 'ok' : 'missing' };
}
