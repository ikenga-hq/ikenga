// WP-74b: the remote client's read model (G-ACCESS §3.12, §5.7, D-7).

import { describe, expect, it } from 'vitest';

import type { AccessStatus } from '@/lib/access/client';
import type { ChiCacheRow, TerminalDescriptor } from '@/lib/tauri-cmd';

import {
	type AnnotatedRow,
	agentCliOf,
	dispatchTargets,
	foregroundRefusal,
	inboxReadOnlyReason,
	sessionRows,
} from './remote-model';

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
		expect(rows[0]?.target).toMatchObject({ kind: 'pty', ptyId: 'p1', agent: 'claude' });
		expect(rows[1]?.target).toMatchObject({ kind: 'chi', runId: 'run-12345678' });
	});

	it('a plain shell is a session but never a dispatch target', () => {
		const terms = [
			{
				terminal_id: 'b',
				pty_id: 'bash-1',
				title: 'bash -l',
				label: null,
				cwd: '/w',
				argv: ['bash', '-l'],
				status: 'running',
				pid: 2,
				foreground_command: { pid: 2, name: 'bash', args: ['bash', '-l'] },
				owner_agent_id: null,
			},
		] as TerminalDescriptor[];
		const rows = sessionRows(terms, []);
		expect(rows).toHaveLength(1);
		expect(rows[0]?.target).toBeNull();
		expect(dispatchTargets(rows)).toEqual([]);
	});

	it('the served foreground snapshot wins over the descriptor', () => {
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
		] as TerminalDescriptor[];
		const rows = sessionRows(terms, [], { p1: { pid: 1, name: 'bash', args: ['bash'] } });
		expect(dispatchTargets(rows)).toEqual([]);
	});
});

describe('agentCliOf', () => {
	const fg = (name: string, args: string[] = []) => ({ pid: 1, name, args });

	it('matches the agent CLIs by process name', () => {
		for (const n of ['claude', 'codex', 'agy', 'opencode', 'pi']) expect(agentCliOf(fg(n))).toBe(n);
		expect(agentCliOf(fg('codex-x86_64-un'))).toBe('codex');
		expect(agentCliOf(fg('.opencode'))).toBe('opencode');
	});

	it('matches node/bun-wrapped CLIs by their entry script', () => {
		expect(
			agentCliOf(fg('node', ['node', '/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js']))
		).toBe('claude');
		expect(agentCliOf(fg('node', ['/usr/bin/node', '/opt/bin/codex.js', '--yolo']))).toBe('codex');
		expect(
			agentCliOf(fg('node', ['node', '/x/node_modules/@mariozechner/pi-coding-agent/dist/cli.js']))
		).toBe('pi');
		expect(agentCliOf(fg('MainThread', ['node', '--no-warnings', '/x/bin/opencode']))).toBe(
			'opencode'
		);
		expect(agentCliOf(fg('node', ['node', '/usr/bin/npx', '@openai/codex@latest']))).toBe('codex');
	});

	it('is fail-closed for shells, editors and arguments', () => {
		for (const n of ['bash', 'zsh', 'fish', 'sh', 'vim', 'python3', 'pip', 'claudette']) {
			expect(agentCliOf(fg(n))).toBeNull();
		}
		expect(agentCliOf(fg('bash', ['bash', '-c', 'claude']))).toBeNull();
		expect(agentCliOf(fg('node', ['node', 'build.js', 'codex']))).toBeNull();
		expect(agentCliOf(fg('node', ['node']))).toBeNull();
		expect(agentCliOf(null)).toBeNull();
		expect(agentCliOf(undefined)).toBeNull();
	});

	it('foregroundRefusal names what replaced the agent', () => {
		const t = {
			kind: 'pty' as const,
			key: 'pty:p',
			ptyId: 'p',
			agent: 'claude' as const,
			label: 'claude · w',
		};
		expect(foregroundRefusal(t, fg('claude'))).toBeNull();
		expect(foregroundRefusal(t, fg('bash'))).toMatch(/no longer running an agent.*`bash`/);
		expect(foregroundRefusal(t, null)).toMatch(/no longer running an agent/);
	});
});

describe('sessionRows with no foreground snapshot', () => {
	it('falls back to the descriptor when the snapshot is null (older daemon)', () => {
		const rows = sessionRows(
			[
				{
					pty_id: 'p1',
					label: 'claude · session 1',
					title: '',
					argv: ['claude'],
					status: 'running',
					foreground_command: { pid: 1, name: 'claude', args: ['claude'] },
				} as never,
			],
			[],
			null
		);
		expect(rows.map((r) => r.label)).toEqual(['claude · session 1']);
		expect(rows[0].target).not.toBeNull();
	});
});
