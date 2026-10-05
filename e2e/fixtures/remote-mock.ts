// Browser-mode remote host mock (WP-78b): the page as a browser sees it when
// `ikenga-server` serves it — no `__TAURI_INTERNALS__`, every command over
// `POST /api/rpc`, the tier from `GET /api/health`, the T1 principal from
// `GET /auth/me` (G-PRINCIPAL §2.4), a paired device's grant from the
// `access_status` probe (G-ACCESS §2.4, §3.8).
//
// Same response table as `tauri-mock.ts` (keyed by command name, with the
// `__error` / `__byArg` shapes), answered from Node by `page.route`, so the
// real `WebRemoteTransport` runs unchanged. The WebSocket channels
// (`/ws/pty`, `/ws/fs`) are accepted and left silent.

import type { Page } from '@playwright/test';

import { DEFAULT_RESPONSES, type MockResponses } from './tauri-mock';

export interface RemoteMockOptions {
	/** What `/api/health` reports (`executor.tier`). */
	tier: 't0' | 't1';
	/** T1: the signed-in principal (`/auth/me`), or `null` for signed out. */
	me?: { principal_id: string; username: string; is_admin: boolean } | null;
	/** Merged over `DEFAULT_RESPONSES`. */
	responses?: MockResponses;
}

export interface RemoteMock {
	/** Commands posted to `/api/rpc`, in order. */
	calls: { cmd: string; args: unknown }[];
	/** Change one canned answer from now on. */
	respond: (cmd: string, value: unknown) => void;
}

function resolve(responses: MockResponses, cmd: string, args: Record<string, unknown>) {
	if (!Object.hasOwn(responses, cmd)) return { ok: true, data: null };
	let v = responses[cmd] as any;
	if (v && typeof v === 'object' && '__byArg' in v) {
		const spec = v.__byArg as { key: string; values: Record<string, unknown>; fallback?: unknown };
		const raw = args?.[spec.key];
		const arg = typeof raw === 'string' ? raw : raw === undefined ? undefined : JSON.stringify(raw);
		v = arg !== undefined && Object.hasOwn(spec.values, arg) ? spec.values[arg] : spec.fallback;
	}
	if (v && typeof v === 'object' && '__error' in v) return { ok: false, error: String(v.__error) };
	return { ok: true, data: v === undefined ? null : v };
}

/** Serve the page as `ikenga-server` would. Call before `page.goto`. */
export async function installRemoteMock(page: Page, opts: RemoteMockOptions): Promise<RemoteMock> {
	const responses: MockResponses = { ...DEFAULT_RESPONSES, ...(opts.responses ?? {}) };
	const calls: RemoteMock['calls'] = [];
	let me = opts.me ?? null;

	// Nothing leaves the preview server (fonts, CDNs): see `installTauriMock`.
	await page.route(
		(url) => url.hostname !== '127.0.0.1' && url.hostname !== 'localhost',
		(route) => route.abort('blockedbyclient')
	);
	await page.route('**/api/health', (route) =>
		route.fulfill({ json: { ok: true, executor: { tier: opts.tier } } })
	);
	await page.route('**/auth/me', (route) =>
		me
			? route.fulfill({ json: me })
			: route.fulfill({ status: 401, json: { error: 'unauthorized' } })
	);
	await page.route('**/auth/logout', (route) => {
		me = null;
		return route.fulfill({ status: 204, body: '' });
	});
	await page.route('**/api/rpc', async (route) => {
		const body = (route.request().postDataJSON() ?? {}) as {
			cmd?: string;
			args?: Record<string, unknown>;
		};
		const cmd = body.cmd ?? '';
		calls.push({ cmd, args: body.args ?? {} });
		return route.fulfill({ json: resolve(responses, cmd, body.args ?? {}) });
	});
	await page.routeWebSocket(/\/ws\//, () => {
		// Accepted and silent: no terminal output, no file events.
	});
	return {
		calls,
		respond: (cmd, value) => {
			responses[cmd] = value;
		},
	};
}
