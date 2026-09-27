// Devices / Remote access — pure model (WP-72, D-05 `devices`, read-only).
//
// This reads only what the daemon already exposes. On the desktop that is the
// `DaemonInfo` the shell got when it found or spawned `ikenga-server`
// (`pty_daemon_info`): host, port, token, pid, persistent or ephemeral. In a
// browser session served by the daemon, it is the page's own origin. Nothing
// here pairs, grants or revokes. There is no device identity to do it with:
// one bearer token stands in for every client (Round 45 risk, G-ACCESS).

import type { DaemonInfo } from '@/lib/tauri-cmd';

export type Exposure =
	| 'loopback'
	| 'tailnet'
	| 'lan'
	| 'all-interfaces'
	| 'public'
	| 'unknown';

export interface ExposureCopy {
	label: string;
	note: string;
	tone: 'muted' | 'live' | 'warn' | 'danger';
}

export const EXPOSURE_COPY: Readonly<Record<Exposure, ExposureCopy>> = {
	loopback: {
		label: 'This machine only',
		note: 'Bound to loopback. Nothing else can reach it, not even your tailnet.',
		tone: 'muted',
	},
	tailnet: {
		label: 'Tailscale only',
		note: 'Only machines on your tailnet can see it. Nothing is published to the internet.',
		tone: 'live',
	},
	lan: {
		label: 'LAN',
		note: 'Anyone on the same local network can reach the address. The token is the only gate.',
		tone: 'warn',
	},
	'all-interfaces': {
		label: 'Every interface',
		note: 'Bound to all interfaces: loopback, LAN and tailnet alike. The token is the only gate.',
		tone: 'warn',
	},
	public: {
		label: 'Public address',
		note: 'Bound to a public address. Anyone who can route to it is stopped only by the token.',
		tone: 'danger',
	},
	unknown: {
		label: 'Unknown',
		note: "The bind address isn't one this view recognises. Check how the daemon was started.",
		tone: 'warn',
	},
};

function ipv4(host: string): [number, number, number, number] | null {
	const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host);
	if (!m) return null;
	const parts = [m[1], m[2], m[3], m[4]].map(Number) as [number, number, number, number];
	return parts.every((p) => p >= 0 && p <= 255) ? parts : null;
}

/** Classify a daemon bind host or page hostname by who can reach it. */
export function classifyHost(raw: string): Exposure {
	const host = raw.trim().toLowerCase().replace(/^\[|\]$/g, '');
	if (!host) return 'unknown';
	if (host === 'localhost' || host === '::1' || host.endsWith('.localhost')) return 'loopback';
	if (host === '0.0.0.0' || host === '::') return 'all-interfaces';
	if (host.endsWith('.ts.net')) return 'tailnet';
	// Tailscale's IPv6 ULA range, fd7a:115c:a1e0::/48.
	if (host.startsWith('fd7a:115c:a1e0:')) return 'tailnet';
	if (host.endsWith('.local') || host.endsWith('.lan') || host.endsWith('.home.arpa')) return 'lan';
	const v4 = ipv4(host);
	if (v4) {
		const [a, b] = v4;
		if (a === 127) return 'loopback';
		// Tailscale's CGNAT range, 100.64.0.0/10.
		if (a === 100 && b >= 64 && b <= 127) return 'tailnet';
		if (a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168)) return 'lan';
		if (a === 169 && b === 254) return 'lan';
		return 'public';
	}
	if (host.includes(':')) {
		// Other IPv6: link-local and unique-local are LAN, the rest routable.
		if (host.startsWith('fe80:') || host.startsWith('fc') || host.startsWith('fd')) return 'lan';
		return 'public';
	}
	return 'unknown';
}

/** Show that a token exists without showing it: `••••••••3f9a`. */
export function maskToken(token: string | null | undefined): string {
	const t = (token ?? '').trim();
	if (!t) return '';
	if (t.length < 12) return '••••••••';
	return `••••••••${t.slice(-4)}`;
}

export type DaemonRun = 'running' | 'stopped' | 'unknown';

export interface DevicesView {
	/** Where this view read from. */
	source: 'desktop' | 'remote' | 'none';
	run: DaemonRun;
	/** Why `run` is what it is, in one line. */
	runNote: string;
	/** `http://host:port`, or null when nothing serves. */
	address: string | null;
	host: string | null;
	exposure: Exposure;
	tailnetAddress: string | null;
	tokenPresent: boolean;
	tokenMasked: string;
	pid: number | null;
	mode: 'persistent' | 'ephemeral' | null;
}

export type ProbeResult = 'up' | 'down' | 'skipped';

/**
 * Build the read-only view.
 * - `info`: `ptyDaemonInfo()` on the desktop; `null` there means the command
 *   failed.
 * - `remote`: the page origin, when this is a browser session on the daemon.
 * - `probe`: a fresh `/api/health` check, since `info` dates from launch and
 *   the daemon may have idled out since.
 */
export function buildDevicesView(
	info: DaemonInfo | null,
	remote: { origin: string; hostname: string; hasToken: boolean } | null,
	probe: ProbeResult
): DevicesView {
	if (remote) {
		const exposure = classifyHost(remote.hostname);
		return {
			source: 'remote',
			run: 'running',
			runNote: 'This page is being served by it.',
			address: remote.origin,
			host: remote.hostname,
			exposure,
			tailnetAddress: exposure === 'tailnet' ? remote.hostname : null,
			tokenPresent: remote.hasToken,
			tokenMasked: remote.hasToken ? '••••••••' : '',
			pid: null,
			mode: 'persistent',
		};
	}
	if (!info) {
		return {
			source: 'none',
			run: 'unknown',
			runNote: "This window can't read the daemon's state.",
			address: null,
			host: null,
			exposure: 'unknown',
			tailnetAddress: null,
			tokenPresent: false,
			tokenMasked: '',
			pid: null,
			mode: null,
		};
	}
	const persistent = info.available && info.mode === 'persistent';
	const exposure = classifyHost(info.host);
	let run: DaemonRun;
	let runNote: string;
	if (!persistent) {
		run = 'stopped';
		runNote = 'No daemon. Terminals run inside the app and nothing is served to other devices.';
	} else if (probe === 'down') {
		run = 'stopped';
		runNote = 'It was running at launch and has stopped answering (it idles out when unused).';
	} else if (probe === 'up') {
		run = 'running';
		runNote = 'Answered a health check just now.';
	} else {
		run = 'running';
		runNote = 'Running at launch.';
	}
	return {
		source: 'desktop',
		run,
		runNote,
		address: persistent ? info.httpUrl || `http://${info.host}:${info.port}` : null,
		host: persistent ? info.host : null,
		exposure: persistent ? exposure : 'loopback',
		tailnetAddress: persistent && exposure === 'tailnet' ? info.host : null,
		tokenPresent: persistent && Boolean(info.token),
		tokenMasked: persistent ? maskToken(info.token) : '',
		pid: info.pid ?? null,
		mode: info.mode,
	};
}

/**
 * Is anything answering at `httpUrl`? A `no-cors` fetch is enough: an opaque
 * response means something is listening, and a network error means nothing
 * is. The daemon's CORS list needn't include this window's origin.
 */
export async function probeDaemon(
	httpUrl: string,
	timeoutMs = 1500,
	fetchImpl: typeof fetch = fetch
): Promise<ProbeResult> {
	const controller = new AbortController();
	const timer = setTimeout(() => controller.abort(), timeoutMs);
	try {
		await fetchImpl(`${httpUrl.replace(/\/+$/, '')}/api/health`, {
			mode: 'no-cors',
			cache: 'no-store',
			signal: controller.signal,
		});
		return 'up';
	} catch {
		return 'down';
	} finally {
		clearTimeout(timer);
	}
}
