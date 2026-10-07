import { describe, expect, it } from 'vitest';
import type { TerminalTab } from '@/terminal/session-store';
import {
	isWslTab,
	normalizeWslDistro,
	planWslRelaunch,
	scansForNetworkErrno,
	snapshotWslSessions,
	wslShutdownSettled,
	wslTabDistro,
} from './tabs';

function tab(over: Partial<TerminalTab> & { id: string }): TerminalTab {
	return {
		title: over.id,
		spec: { cwd: '/', cmd: ['pwsh.exe'] },
		ptyId: null,
		status: 'running',
		exitCode: null,
		createdAt: 0,
		owner: { kind: 'sidepane' },
		...over,
	};
}

const claudeWsl = (id: string, distro: string | null, sid: string | null) =>
	tab({
		id,
		spec: { cwd: '/', cmd: ['wsl.exe'], wrap: { shellTarget: 'wsl', wslDistro: distro } },
		ptyId: `pty-${id}`,
		claudeSessionId: sid,
	});

describe('WSL tab detection', () => {
	it('recognises wrapped agents and plain wsl.exe shells', () => {
		expect(isWslTab(claudeWsl('a', 'Ubuntu', null))).toBe(true);
		expect(
			isWslTab(tab({ id: 'b', spec: { cwd: '/', cmd: ['C:\\Windows\\System32\\wsl.exe'] } }))
		).toBe(true);
		expect(isWslTab(tab({ id: 'c', spec: { cwd: '/', cmd: ['pwsh.exe'] } }))).toBe(false);
		expect(
			isWslTab(
				tab({ id: 'd', spec: { cwd: '/', cmd: ['claude'], wrap: { shellTarget: 'native' } } })
			)
		).toBe(false);
	});

	it('reads the distro from the wrap or from -d / --distribution', () => {
		expect(wslTabDistro(claudeWsl('a', 'Debian', null))).toBe('Debian');
		expect(wslTabDistro(claudeWsl('a', null, null))).toBe('default');
		expect(wslTabDistro(claudeWsl('a', ' Default ', null))).toBe('default');
		expect(
			wslTabDistro(tab({ id: 'b', spec: { cwd: '/', cmd: ['wsl.exe', '-d', 'Ubuntu-24.04'] } }))
		).toBe('Ubuntu-24.04');
		expect(
			wslTabDistro(tab({ id: 'b', spec: { cwd: '/', cmd: ['wsl', '--distribution', 'Arch'] } }))
		).toBe('Arch');
		// A -d after -e belongs to the command, not wsl.exe.
		expect(
			wslTabDistro(tab({ id: 'b', spec: { cwd: '/', cmd: ['wsl.exe', '-e', 'ls', '-d', 'x'] } }))
		).toBe('default');
	});

	it('normalises blank / default to "default"', () => {
		expect(normalizeWslDistro(null)).toBe('default');
		expect(normalizeWslDistro('')).toBe('default');
		expect(normalizeWslDistro('DEFAULT')).toBe('default');
		expect(normalizeWslDistro(' Ubuntu ')).toBe('Ubuntu');
	});
});

describe('scansForNetworkErrno', () => {
	it('scans WSL tabs on a fresh spawn, never on a reattach (old scrollback)', () => {
		const w = claudeWsl('w', 'Ubuntu', null);
		expect(scansForNetworkErrno(w, { attached: false })).toBe(true);
		expect(scansForNetworkErrno(w, { attached: true })).toBe(false);
		const native = tab({ id: 'n', spec: { cwd: '/', cmd: ['pwsh.exe'] } });
		expect(scansForNetworkErrno(native, { attached: false })).toBe(false);
	});
});

describe('D-5 relaunch snapshot / restore', () => {
	const before = [
		claudeWsl('w1', 'Ubuntu', 'sess-1'),
		claudeWsl('w2', null, null),
		tab({ id: 'w3', spec: { cwd: '/', cmd: ['wsl.exe'] }, status: 'exited', ptyId: null }),
		tab({ id: 'native', spec: { cwd: '/', cmd: ['pwsh.exe'] }, ptyId: 'pty-n' }),
	];

	it('snapshots every WSL tab with its resume id, before the fix', () => {
		expect(snapshotWslSessions(before)).toEqual([
			{
				tabId: 'w1',
				title: 'w1',
				distro: 'Ubuntu',
				claudeSessionId: 'sess-1',
				wasRunning: true,
				ephemeral: false,
			},
			{
				tabId: 'w2',
				title: 'w2',
				distro: 'default',
				claudeSessionId: null,
				wasRunning: true,
				ephemeral: false,
			},
			{
				tabId: 'w3',
				title: 'w3',
				distro: 'default',
				claudeSessionId: null,
				wasRunning: false,
				ephemeral: false,
			},
		]);
	});

	it('relaunches the killed tabs with the snapshotted id, even though exit cleared it', () => {
		const snap = snapshotWslSessions(before);
		// After `wsl --shutdown`: the exit handler cleared ptyId + claudeSessionId.
		const after = before.map((t) =>
			t.id === 'native'
				? t
				: { ...t, status: 'exited' as const, ptyId: null, claudeSessionId: null }
		);
		expect(wslShutdownSettled(snap, after)).toBe(true);
		expect(planWslRelaunch(snap, after)).toEqual([
			{ tabId: 'w1', claudeSessionId: 'sess-1', forceEphemeral: false },
			{ tabId: 'w2', claudeSessionId: null, forceEphemeral: false },
		]);
	});

	it('keeps an in-process (seat / attach-run) PTY in-process on relaunch', () => {
		const seat = { ...claudeWsl('seat', 'Ubuntu', 'sess-s'), mode: 'ephemeral' as const };
		const snap = snapshotWslSessions([seat]);
		expect(snap[0]?.ephemeral).toBe(true);
		const after = [{ ...seat, status: 'exited' as const, ptyId: null, claudeSessionId: null }];
		expect(planWslRelaunch(snap, after)).toEqual([
			{ tabId: 'seat', claudeSessionId: 'sess-s', forceEphemeral: true },
		]);
	});

	it('skips closed tabs and tabs the shutdown did not kill', () => {
		const snap = snapshotWslSessions(before);
		const after = [
			// w1 closed by the user meanwhile; w2 still running.
			before[1],
			before[2],
		];
		expect(wslShutdownSettled(snap, after)).toBe(false);
		expect(planWslRelaunch(snap, after)).toEqual([]);
	});

	it('nothing to relaunch from an empty snapshot', () => {
		expect(planWslRelaunch([], before)).toEqual([]);
		expect(wslShutdownSettled([], before)).toBe(true);
	});
});
