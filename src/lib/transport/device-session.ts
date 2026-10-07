/**
 * Paired-device browser sessions (G-ACCESS §2.4, §3.8, §3.12; WP-74b).
 *
 * A paired device holds no bearer token: its credential is the HttpOnly
 * `ikenga_device` cookie the pairing status poll set, which page JS can't
 * read. Same-origin `fetch` and `new WebSocket` send it on their own, so the
 * transport needs only to know that this tab IS a remote session. The boot
 * path asks once (`access_status` over `/api/rpc`, cookie only) and records
 * the answer here.
 *
 * No imports, like `t1-session.ts`, so the boot path can load it before the
 * transport picks a backend.
 */

/** The subset of `AccessStatus` (§9.1) the boot path needs. */
export interface BootAccessStatus {
	tier: 't0' | 't1';
	credential: { via: 'session' | 'device' | 'operator'; deviceId: string | null; tier: string };
	caps: string[];
	adminStrength: boolean;
}

let device = false;
let status: BootAccessStatus | null = null;

/** True once {@link detectAccessStatus} saw a device credential. */
export function isDeviceSession(): boolean {
	return device;
}

/** What the boot probe read, if it ran and succeeded. */
export function bootAccessStatus(): BootAccessStatus | null {
	return status;
}

/**
 * plans/pwa S1 (W2): the server could not be reached at all — a network
 * error, the browser offline, or a gateway in front of a stopped daemon.
 * Distinct from `null` (reached, but no credential) so the boot path shows
 * "can't reach the server" rather than offering to pair a device that is
 * already paired.
 */
export const UNREACHABLE = 'unreachable' as const;

/** What the boot probe found. */
export type AccessProbe = BootAccessStatus | null | typeof UNREACHABLE;

/** A gateway (tailscale serve, a reverse proxy) answering for a daemon that is down. */
const GATEWAY_DOWN = new Set([502, 503, 504]);

function browserOffline(): boolean {
	return typeof navigator !== 'undefined' && navigator.onLine === false;
}

/**
 * `access_status` with whatever the browser sends by itself (cookies), plus
 * the T0 token when the tab has one. `null` on 401 or a server too old to
 * know the arm; {@link UNREACHABLE} when no server answered. Never throws.
 */
export async function detectAccessStatus(token: string | null = null): Promise<AccessProbe> {
	try {
		const headers: Record<string, string> = { 'Content-Type': 'application/json' };
		if (token) headers.Authorization = `Bearer ${token}`;
		const res = await fetch('/api/rpc', {
			method: 'POST',
			headers,
			credentials: 'same-origin',
			body: JSON.stringify({ cmd: 'access_status', args: {} }),
		});
		if (GATEWAY_DOWN.has(res.status)) return UNREACHABLE;
		if (!res.ok) return null;
		const json = (await res.json()) as { ok?: boolean; data?: BootAccessStatus } | null;
		if (!json?.ok || !json.data?.credential) return null;
		status = json.data;
		device = json.data.credential.via === 'device';
		return status;
	} catch (err) {
		// `fetch` rejects with a TypeError only when no response arrived.
		if (err instanceof TypeError || browserOffline()) return UNREACHABLE;
		return null;
	}
}

/** P-21: a device grant below `full` boots into `/remote`. */
export function bootsIntoRemote(s: AccessProbe): boolean {
	if (s === null || s === UNREACHABLE) return false;
	return s.credential.via === 'device' && s.credential.tier !== 'full';
}

/** Test seam. */
export function _resetDeviceSessionForTests(): void {
	device = false;
	status = null;
}
