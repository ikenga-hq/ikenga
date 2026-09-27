// WP-69 (G-SEATS §4.4) — a terminal attached to a persistent run's tmux
// session.
import { beforeEach, describe, expect, it, vi } from 'vitest';

const m = vi.hoisted(() => ({
	chiList: vi.fn(async (): Promise<unknown[]> => []),
	openTabPty: vi.fn(async () => ({})),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	chiList: m.chiList,
	settingsGet: vi.fn(async () => null),
}));
vi.mock('./session-store', async (orig) => ({
	...(await orig<typeof import('./session-store')>()),
	openTabPty: m.openTabPty,
}));

import { queryClient } from '@/lib/query-client';
import {
	attachRunTerminal,
	cachedRunTerminalSession,
	fetchRunTerminalSession,
	findRunAttachTerminal,
	isRunAttachCmd,
	RUN_LOOKUP_LIMIT,
	runAttachArgv,
	runTerminalSessionKey,
} from './attach-run';
import { type TerminalTab, useTerminalStore } from './session-store';

function tab(id: string, cmd: string[], status: TerminalTab['status'] = 'running'): TerminalTab {
	return {
		id,
		title: id,
		spec: { cwd: '/w', cmd },
		ptyId: `pty-${id}`,
		status,
		exitCode: null,
		createdAt: 0,
		owner: { kind: 'sidepane' },
	};
}

beforeEach(() => {
	queryClient.clear();
	m.chiList.mockReset().mockResolvedValue([]);
	m.openTabPty.mockReset().mockResolvedValue({});
	useTerminalStore.setState({ tabs: [] });
});

describe('the tmux client argv', () => {
	it('targets the session by exact name, the form Rust reads as the run’s mount', () => {
		expect(runAttachArgv('run-1')).toEqual(['tmux', 'attach-session', '-t', '=run-1']);
		expect(isRunAttachCmd(runAttachArgv('run-1'), 'run-1')).toBe(true);
		expect(isRunAttachCmd(runAttachArgv('run-12'), 'run-1')).toBe(false);
		expect(isRunAttachCmd(['bash'], 'run-1')).toBe(false);
	});

	it('finds only a running attach terminal for that session', () => {
		const tabs = [tab('a', runAttachArgv('run-1'), 'exited'), tab('b', runAttachArgv('run-1')), tab('c', ['bash'])];
		expect(findRunAttachTerminal(tabs, 'run-1')?.id).toBe('b');
		expect(findRunAttachTerminal(tabs, 'run-2')).toBeNull();
	});
});

describe('the run’s tmux session (chi_cache.terminal_session_id)', () => {
	it('is the row’s terminal_session_id, looked up among the engine’s runs', async () => {
		m.chiList.mockResolvedValue([
			{ run_id: 'other', terminal_session_id: 'other' },
			{ run_id: 'run-1', terminal_session_id: 'run-1' },
		]);
		await expect(fetchRunTerminalSession({ runId: 'run-1', engineId: 'claude-code' })).resolves.toBe('run-1');
		expect(m.chiList).toHaveBeenCalledWith('claude-code', RUN_LOOKUP_LIMIT);
	});

	it('is null for a one-off run or a run no longer cached', async () => {
		m.chiList.mockResolvedValue([{ run_id: 'run-1' }]);
		await expect(fetchRunTerminalSession({ runId: 'run-1', engineId: 'x' })).resolves.toBeNull();
		await expect(fetchRunTerminalSession({ runId: 'gone', engineId: 'x' })).resolves.toBeNull();
	});

	it('reads undefined until fetched, then the cached value', async () => {
		m.chiList.mockResolvedValue([{ run_id: 'run-1', terminal_session_id: 'run-1' }]);
		const run = { runId: 'run-1', engineId: 'claude-code' };
		expect(cachedRunTerminalSession(run)).toBeUndefined();
		await vi.waitFor(() => expect(queryClient.getQueryData(runTerminalSessionKey(run))).toBe('run-1'));
		expect(cachedRunTerminalSession(run)).toBe('run-1');
	});
});

describe('attachRunTerminal', () => {
	it('reuses a running attach terminal', async () => {
		useTerminalStore.setState({ tabs: [tab('att', runAttachArgv('run-1'))] });
		await expect(attachRunTerminal({ session: 'run-1', cwd: '/w', title: 'nightly · run' })).resolves.toBe('att');
		expect(m.openTabPty).not.toHaveBeenCalled();
	});

	it('spawns one in-process terminal, once, for concurrent calls', async () => {
		const opts = { session: 'run-1', cwd: '/w', title: 'nightly · run' };
		const [a, b] = await Promise.all([attachRunTerminal(opts), attachRunTerminal(opts)]);
		expect(a).toBe(b);
		expect(m.openTabPty).toHaveBeenCalledTimes(1);
		expect(m.openTabPty.mock.calls[0][1]).toEqual({ forceEphemeral: true });
		const t = useTerminalStore.getState().tabs.find((x) => x.id === a);
		expect(t?.spec.cmd).toEqual(runAttachArgv('run-1'));
		expect(t?.title).toBe('nightly · run');
	});

	it('removes the tab when the spawn fails', async () => {
		m.openTabPty.mockRejectedValue(new Error('no tmux'));
		await expect(attachRunTerminal({ session: 'run-1', cwd: '/w', title: 't' })).rejects.toThrow('no tmux');
		expect(useTerminalStore.getState().tabs).toEqual([]);
	});
});
