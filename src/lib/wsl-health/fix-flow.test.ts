// honest-failure-states WP-2 — D-5 around the fixes that shut WSL down:
// snapshot resume ids before the call, relaunch with them after `done`,
// leave sessions alone on `cancelled_by_user`.

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { create, type StoreApi, type UseBoundStore } from 'zustand';
import type { WslFixOutcome, WslHealth } from '@/lib/tauri-cmd';
import type { TerminalTab } from '@/terminal/session-store';

const mocks = vi.hoisted(() => ({
	wslHealthFix: vi.fn(),
	openTabPty: vi.fn(),
	setWslHealth: vi.fn(),
}));

interface MiniStore {
	tabs: TerminalTab[];
	setClaudeSessionId: (id: string, sid: string | null) => void;
	setStatus: (id: string, status: TerminalTab['status']) => void;
}

const store = vi.hoisted(() => ({ ref: null as unknown as UseBoundStore<StoreApi<MiniStore>> }));

vi.mock('@/lib/tauri-cmd', () => ({ wslHealthFix: mocks.wslHealthFix }));
vi.mock('./query', () => ({ setWslHealth: mocks.setWslHealth }));
vi.mock('@/terminal/session-store', () => ({
	openTabPty: mocks.openTabPty,
	get useTerminalStore() {
		return store.ref;
	},
}));

import { confirmWslFix, requestWslFix, runWslFix } from './fix-flow';
import { useWslHealthUi } from './store';

function wslTab(id: string, sid: string | null): TerminalTab {
	return {
		id,
		title: id,
		spec: { cwd: '/', cmd: ['wsl.exe'], wrap: { shellTarget: 'wsl', wslDistro: 'Ubuntu' } },
		claudeSessionId: sid,
		ptyId: `pty-${id}`,
		status: 'running',
		exitCode: null,
		createdAt: 0,
		owner: { kind: 'sidepane' },
	};
}

const OK: WslHealth = {
	state: 'ok',
	distro: 'Ubuntu',
	detail: '',
	mirroredFailure: null,
	networkingMode: 'mirrored',
	checkedAt: 1,
};

/** What `wsl --shutdown` does to the store: every WSL PTY exits and the exit
 *  handler clears its resume id. */
function killAllWsl() {
	store.ref.setState((s) => ({
		tabs: s.tabs.map((t) => ({
			...t,
			status: 'exited' as const,
			ptyId: null,
			claudeSessionId: null,
		})),
	}));
}

beforeEach(() => {
	vi.clearAllMocks();
	store.ref = create<MiniStore>((set) => ({
		tabs: [wslTab('a', 'sess-a'), wslTab('b', null)],
		setClaudeSessionId: (id, sid) =>
			set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, claudeSessionId: sid } : t)) })),
		setStatus: (id, status) =>
			set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, status } : t)) })),
	}));
	useWslHealthUi.setState({ confirm: null, runs: {}, restartTried: {}, dismissed: {} });
	mocks.openTabPty.mockResolvedValue({});
});

describe('requestWslFix', () => {
	it('runs repair_dns at once, with no confirm', async () => {
		mocks.wslHealthFix.mockResolvedValue({ outcome: 'done', health: OK } satisfies WslFixOutcome);
		requestWslFix('repair_dns', null);
		expect(useWslHealthUi.getState().confirm).toBeNull();
		await vi.waitFor(() =>
			expect(mocks.wslHealthFix).toHaveBeenCalledWith('repair_dns', 'default')
		);
	});

	it('opens the confirm (listing WSL sessions) for disruptive fixes', () => {
		requestWslFix('restart_networking', 'Ubuntu');
		const c = useWslHealthUi.getState().confirm;
		expect(c?.action).toBe('restart_networking');
		expect(c?.sessions.map((s) => s.tabId)).toEqual(['a', 'b']);
		expect(mocks.wslHealthFix).not.toHaveBeenCalled();
	});
});

describe('D-5 relaunch', () => {
	it('done: relaunches each killed tab with its snapshotted resume id', async () => {
		mocks.wslHealthFix.mockImplementation(async () => {
			killAllWsl();
			return { outcome: 'done', health: OK } satisfies WslFixOutcome;
		});
		requestWslFix('restart_networking', 'Ubuntu');
		await confirmWslFix();

		expect(mocks.openTabPty).toHaveBeenCalledTimes(2);
		const spawned = mocks.openTabPty.mock.calls.map(([t]) => [t.id, t.claudeSessionId]);
		expect(spawned).toEqual([
			['a', 'sess-a'],
			['b', null],
		]);
		expect(mocks.setWslHealth).toHaveBeenCalledWith('Ubuntu', OK);
		const run = useWslHealthUi.getState().runs.Ubuntu;
		expect(run?.phase).toBe('done');
		expect(run?.message).toContain('Reopened 2 WSL sessions');
	});

	it('cancelled_by_user: says so and leaves sessions alone', async () => {
		mocks.wslHealthFix.mockResolvedValue({ outcome: 'cancelled_by_user' } satisfies WslFixOutcome);
		requestWslFix('restart_networking', 'Ubuntu');
		await confirmWslFix();
		expect(mocks.openTabPty).not.toHaveBeenCalled();
		expect(store.ref.getState().tabs.map((t) => t.claudeSessionId)).toEqual(['sess-a', null]);
		expect(useWslHealthUi.getState().runs.Ubuntu?.phase).toBe('cancelled');
	});

	it('a restart that leaves WSL broken marks restartTried (NAT is offered next)', async () => {
		mocks.wslHealthFix.mockResolvedValue({
			outcome: 'done',
			health: { ...OK, state: 'no_route' },
		} satisfies WslFixOutcome);
		await runWslFix('restart_networking', 'Ubuntu');
		expect(useWslHealthUi.getState().restartTried.Ubuntu).toBe(true);
		expect(useWslHealthUi.getState().runs.Ubuntu?.message).toMatch(/^That didn't fix it/);
	});

	it('failed: reports the reason, no relaunch', async () => {
		mocks.wslHealthFix.mockResolvedValue({
			outcome: 'failed',
			reason: 'boom',
		} satisfies WslFixOutcome);
		const out = await runWslFix('switch_to_nat', 'Ubuntu', []);
		expect(out.outcome).toBe('failed');
		expect(mocks.openTabPty).not.toHaveBeenCalled();
		expect(useWslHealthUi.getState().runs.Ubuntu?.message).toBe("Couldn't fix it: boom");
	});
});
