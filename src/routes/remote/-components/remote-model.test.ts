// WP-74b: the remote client's read model (G-ACCESS §3.12, §5.7, D-7).

import { describe, expect, it } from 'vitest';

import type { AccessStatus } from '@/lib/access/client';
import type { ChiCacheRow, TerminalDescriptor } from '@/lib/tauri-cmd';

import { type AnnotatedRow, inboxReadOnlyReason, sessionRows } from './remote-model';

function status(
	tier: AccessStatus['credential']['tier'],
	caps: AccessStatus['caps']
): AccessStatus {
	return {
		tier: 't0',
		store: 'ok',
		principal: { principalId: 'p', username: 'ned', isAdmin: false },
		credential: { via: 'device', deviceId: 'd', tier },
		caps,
		adminStrength: false,
		publicUrl: null,
		sharingEnabled: false,
		share: null,
	};
}

const row = (over: Partial<AnnotatedRow> = {}): AnnotatedRow => ({
	id: 1,
	kind: 'permission',
	title: 'claude wants to read .env',
	body: null,
	action: null,
	source: 'iyke.hooks',
	dedupeKey: null,
	count: 1,
	createdAt: 1,
	updatedAt: 1,
	readAt: null,
	resolvedAt: null,
	...over,
});

describe('inboxReadOnlyReason', () => {
	const dispatch = status('dispatch', ['files', 'sessions', 'dispatch']);
	const approve = status('approve', ['files', 'sessions', 'dispatch', 'approve']);

	it('is live only when the row says can_decide', () => {
		expect(inboxReadOnlyReason(row({ can_decide: true }), approve)).toBeNull();
		expect(inboxReadOnlyReason(row(), approve)).toBe('Answer this on the computer for now');
	});

	it('D-7: a dispatch device reads the inbox with its reason', () => {
		expect(inboxReadOnlyReason(row(), dispatch)).toBe(
			"This device can't approve — it is View + dispatch"
		);
		expect(inboxReadOnlyReason(row({ can_decide: false, waiting_on: 'owner' }), approve)).toBe(
			'Waiting on the Owner'
		);
		expect(inboxReadOnlyReason(row({ resolvedAt: 5, can_decide: true }), approve)).toBe(
			'Already answered'
		);
	});
});

describe('sessionRows', () => {
	it('lists live terminals, then recent runs', () => {
		const terms = [
			{
				terminal_id: 't',
				pty_id: 'p1',
				title: 'claude',
				label: null,
				cwd: '/w',
				argv: ['claude'],
				status: 'running',
				pid: 1,
				foreground_command: { pid: 1, name: 'claude', args: [] },
				owner_agent_id: null,
			},
			{
				terminal_id: 'x',
				pty_id: 'p2',
				title: 'old',
				label: null,
				cwd: '/',
				argv: [],
				status: 'exited',
				pid: null,
				foreground_command: null,
				owner_agent_id: null,
			},
		] as TerminalDescriptor[];
		const runs = [
			{
				run_id: 'run-12345678',
				engine_id: 'codex',
				status: 'running',
				owner: 'ned',
				brief: 'nightly',
			},
		] as ChiCacheRow[];
		const rows = sessionRows(terms, runs);
		expect(rows.map((r) => r.label)).toEqual(['claude', 'codex · nightly']);
		expect(rows[0]?.ptyId).toBe('p1');
		expect(rows[1]?.ptyId).toBeNull();
	});
});
