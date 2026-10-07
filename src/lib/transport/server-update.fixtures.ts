// Test fixtures for the WP-P9 server-update view (`server-update.ts`).

import type { ServerUpdateRun, ServerUpdateView } from './server-update';

export function serverUpdateView(over: Partial<ServerUpdateView> = {}): ServerUpdateView {
	return {
		supported: true,
		current: '0.20.0',
		available: {
			version: '0.21.0',
			notes_url: 'https://github.com/ikenga-hq/ikenga/releases/tag/v0.21.0',
			published_at: '2026-10-05T12:00:00Z',
			min_upgrade_from: null,
			blocked: false,
			blocked_reason: null,
		},
		checked_at: '2026-10-06T00:00:00Z',
		check_error: null,
		last_run: null,
		pending_request: null,
		open_terminals: 2,
		open_terminals_partial: false,
		can_apply: true,
		apply_blocked_reason: null,
		...over,
	};
}

export function serverUpdateRun(over: Partial<ServerUpdateRun> = {}): ServerUpdateRun {
	const now = new Date().toISOString();
	return {
		state: 'succeeded',
		from: '0.20.0',
		to: '0.21.0',
		request_id: '6f1c7a52-6c1e-4f1c-9c3e-6a1f2b3c4d5e',
		requested_by: 'ada',
		started_at: now,
		finished_at: now,
		rolled_back: false,
		exit_code: 0,
		message: null,
		log_tail: ['==> Upgrading 0.20.0 -> 0.21.0', '    health ok'],
		...over,
	};
}
