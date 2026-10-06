// server-preflight — the row rules for the browser-session report. The
// rendered T0/T1 paths live in `welcome-body.preflight.test.tsx`.

import { describe, expect, it } from 'vitest';

import type { ServerHealth } from '@/lib/tauri-cmd';

import { buildServerChecks, formatUptime, type ServerPreflightInput } from './server-preflight';
import { canContinueFromPreflight } from './welcome-body';

function health(over: Partial<ServerHealth> = {}): ServerHealth {
	return {
		ok: true,
		name: 'ikenga-server',
		version: '0.9.1',
		status: 'ready',
		uptime_secs: 90,
		executor: { tier: 't0', pty: true, piped: true, principal_isolation: false },
		...over,
	};
}

function input(over: Partial<ServerPreflightInput> = {}): ServerPreflightInput {
	return {
		health: health(),
		access: { kind: 'token' },
		claudeProjects: { count: 1 },
		...over,
	};
}

function row(checks: ReturnType<typeof buildServerChecks>, id: string) {
	const found = checks.find((c) => c.id === id);
	if (!found) throw new Error(`no ${id} row`);
	return found;
}

describe('buildServerChecks', () => {
	it('never emits a desktop-only row or a fail', () => {
		const checks = buildServerChecks(input());
		expect(checks.map((c) => c.id)).toEqual(['server', 'access', 'sessions', 'claude_projects']);
		for (const c of checks) {
			expect(c.level).not.toBe('fail');
			expect(`${c.message} ${c.fix_hint ?? ''}`).not.toMatch(/vault|keychain|stronghold/i);
		}
		expect(canContinueFromPreflight({ checks })).toBe(true);
	});

	it('names a paired device', () => {
		const checks = buildServerChecks(input({ access: { kind: 'device' } }));
		expect(row(checks, 'access').message).toBe('This browser is a paired device');
	});

	it('warns when a T1 session cannot confirm who is signed in', () => {
		const checks = buildServerChecks(input({ access: { kind: 't1', principal: null } }));
		expect(row(checks, 'access').level).toBe('warn');
	});

	it('marks an admin principal', () => {
		const checks = buildServerChecks(
			input({
				access: { kind: 't1', principal: { principal_id: 'p', username: 'ola', is_admin: true } },
			})
		);
		expect(row(checks, 'access').message).toBe('Signed in as ola (admin)');
	});

	it('warns that a degraded T1 server refuses sessions (isolation off)', () => {
		const checks = buildServerChecks(
			input({
				health: health({
					executor: { tier: 't1', pty: true, piped: true, principal_isolation: false },
				}),
			})
		);
		const s = row(checks, 'sessions');
		expect(s.level).toBe('warn');
		expect(s.message).toContain('refusing new terminals and agents');
		expect(s.fix_hint).toContain('operator');
	});

	it('warns when the server cannot spawn terminals', () => {
		const checks = buildServerChecks(
			input({
				health: health({
					executor: { tier: 't0', pty: false, piped: true, principal_isolation: false },
				}),
			})
		);
		expect(row(checks, 'sessions')).toMatchObject({
			level: 'warn',
			message: 'This server cannot start terminals',
		});
	});

	it('warns when an older server reports no executor', () => {
		const checks = buildServerChecks(input({ health: health({ executor: undefined }) }));
		expect(row(checks, 'sessions').level).toBe('warn');
	});

	it('warns on a server that is not ready', () => {
		const checks = buildServerChecks(input({ health: health({ status: 'starting' }) }));
		expect(row(checks, 'server')).toMatchObject({ level: 'warn' });
		expect(row(checks, 'server').message).toContain('"starting"');
	});

	it('treats no Claude projects as a gentle warning, and a list error as a warning', () => {
		const none = buildServerChecks(input({ claudeProjects: { count: 0 } }));
		expect(row(none, 'claude_projects').level).toBe('warn');

		const failed = buildServerChecks(input({ claudeProjects: { error: 'missing cap: files' } }));
		expect(row(failed, 'claude_projects')).toMatchObject({ level: 'warn' });
		expect(row(failed, 'claude_projects').message).toContain('missing cap: files');
	});
});

describe('formatUptime', () => {
	it.each([
		[0, 'under a minute'],
		[59, 'under a minute'],
		[60, '1m'],
		[3_660, '1h 1m'],
		[90_000, '1d 1h'],
		[Number.NaN, 'under a minute'],
	])('%s s → %s', (secs, want) => {
		expect(formatUptime(secs)).toBe(want);
	});
});
