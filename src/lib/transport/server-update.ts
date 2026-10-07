// In-app server updates (WP-P9 steps 2–3), the browser client.
//
// The server never upgrades itself: root's units check for a release and
// apply an admin's request (scripts/server/provision.sh). The server answers
// two routes — `GET /api/server/update` and `POST /api/server/update/apply` —
// to the T1 admins (the broker answers them itself) or the T0 operator bearer.
// Everyone else gets a bare 403 and sees nothing.
//
// Deliberately NOT routed through `invoke()`: its 401 handler opens the
// re-auth dialog, and a 403/404 here must stay silent. A desktop window never
// asks — the desktop's own updater (plugin-updater) is a different thing.
//
// Mirrors `src-tauri/src/server/update.rs` (`UpdateView`, `ApplyBody`).

import { isTauri, transportToken } from './index';
import { currentPrincipal, isT1Session } from './t1-session';

export type ServerUpdateBlockedReason =
	| 'unsupported'
	| 'none_available'
	| 'blocked_min_upgrade'
	| 'running'
	| 'pending'
	| 'cooldown';

/** Root's run states, plus `interrupted` (a `running` too old to be live). */
export type ServerUpdateRunState =
	| 'running'
	| 'succeeded'
	| 'noop'
	| 'rolled_back'
	| 'failed'
	| 'refused'
	| 'interrupted';

export interface ServerUpdateAvailable {
	version: string;
	notes_url: string | null;
	published_at: string | null;
	min_upgrade_from: string | null;
	blocked: boolean;
	blocked_reason: string | null;
}

export interface ServerUpdateRun {
	state: ServerUpdateRunState;
	from: string | null;
	to: string | null;
	request_id: string | null;
	requested_by: string | null;
	started_at: string | null;
	finished_at: string | null;
	rolled_back: boolean;
	exit_code: number | null;
	message: string | null;
	log_tail: string[];
}

export interface ServerUpdatePending {
	version: string | null;
	request_id: string | null;
	requested_by: string | null;
	requested_at: string | null;
}

export interface ServerUpdateView {
	supported: boolean;
	/** The server's version (may differ from the SPA bundle this tab loaded). */
	current: string;
	/** The advertised release, only when newer than `current`. */
	available: ServerUpdateAvailable | null;
	checked_at: string | null;
	check_error: string | null;
	last_run: ServerUpdateRun | null;
	pending_request: ServerUpdatePending | null;
	open_terminals: number;
	open_terminals_partial: boolean;
	can_apply: boolean;
	apply_blocked_reason: ServerUpdateBlockedReason | null;
}

export type ApplyServerUpdateResult =
	| { ok: true; version: string; requestId: string }
	| {
			ok: false;
			/** The server's stable code (`terminals_open`, `pending`, `cooldown`,
			 *  `update_running`, `not_advertised`, `blocked`, `throttled`,
			 *  `forbidden`, `unsupported`), or `network`. */
			code: string;
			message: string;
			/** With `terminals_open`: the count that is true now. */
			openTerminals?: number;
	  };

/** Thrown when the server can't be reached (or answers 5xx) — distinct from
 *  "not for you", so a poll during the update's restart keeps the last view. */
export class ServerUnreachableError extends Error {
	constructor(message: string) {
		super(message);
		this.name = 'ServerUnreachableError';
	}
}

/** Whether this tab may ask at all: a browser tab, and under T1 a signed-in
 *  admin (a member is never sent to the route). */
export function mayAskForServerUpdates(): boolean {
	if (isTauri()) return false;
	if (isT1Session()) return currentPrincipal()?.is_admin === true;
	return true;
}

function authHeaders(): Record<string, string> {
	const token = transportToken();
	return token ? { Authorization: `Bearer ${token}` } : {};
}

/**
 * `GET /api/server/update`. `null` when this tab has no business seeing it:
 * the desktop, a T1 member, a refused credential (401/403), a server without
 * the routes or without root's units (404, `supported: false`). Throws
 * {@link ServerUnreachableError} on a network failure or a 5xx.
 */
export async function fetchServerUpdate(): Promise<ServerUpdateView | null> {
	if (!mayAskForServerUpdates()) return null;
	let res: Response;
	try {
		res = await fetch('/api/server/update', {
			headers: authHeaders(),
			credentials: 'same-origin',
		});
	} catch (err) {
		throw new ServerUnreachableError(String(err));
	}
	if (res.status >= 500) throw new ServerUnreachableError(`HTTP ${res.status}`);
	if (!res.ok) return null;
	let body: { ok?: boolean; data?: ServerUpdateView } | null;
	try {
		body = (await res.json()) as typeof body;
	} catch {
		return null;
	}
	const view = body?.ok ? body.data : undefined;
	if (!view || view.supported !== true) return null;
	return view;
}

interface ApplyResponseBody {
	ok?: boolean;
	data?: { version?: string; request_id?: string };
	error?: string;
	code?: string;
	open_terminals?: number;
}

/**
 * `POST /api/server/update/apply`. `acknowledgedOpenTerminals` is the count
 * the admin was shown and confirmed; when more are open now the server
 * answers `terminals_open` with the new count, and the caller re-prompts.
 */
export async function applyServerUpdate(input: {
	version: string;
	acknowledgedOpenTerminals: number;
}): Promise<ApplyServerUpdateResult> {
	if (!mayAskForServerUpdates()) {
		return {
			ok: false,
			code: 'forbidden',
			message: 'Only an administrator can update the server.',
		};
	}
	let res: Response;
	try {
		res = await fetch('/api/server/update/apply', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json', ...authHeaders() },
			credentials: 'same-origin',
			body: JSON.stringify({
				version: input.version,
				acknowledged_open_terminals: Math.max(0, Math.floor(input.acknowledgedOpenTerminals)),
			}),
		});
	} catch (err) {
		return { ok: false, code: 'network', message: `Could not reach the server: ${String(err)}` };
	}
	let body: ApplyResponseBody | null;
	try {
		body = (await res.json()) as ApplyResponseBody | null;
	} catch {
		body = null;
	}
	if (res.ok && body?.ok) {
		return {
			ok: true,
			version: body.data?.version ?? input.version,
			requestId: body.data?.request_id ?? '',
		};
	}
	const code = typeof body?.code === 'string' ? body.code : `http_${res.status}`;
	return {
		ok: false,
		code,
		message: typeof body?.error === 'string' ? body.error : `Request failed (HTTP ${res.status}).`,
		...(typeof body?.open_terminals === 'number' ? { openTerminals: body.open_terminals } : {}),
	};
}

/** A run that is still going (root's `running`; `interrupted` is not). */
export function isServerUpdateInFlight(view: ServerUpdateView | null | undefined): boolean {
	if (!view) return false;
	return view.last_run?.state === 'running' || view.pending_request !== null;
}
