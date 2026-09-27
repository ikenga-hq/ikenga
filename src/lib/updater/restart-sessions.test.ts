import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';
import { useTerminalStore, type TerminalTab } from '@/terminal/session-store';
import { useLiveSessionCount, useLiveSessionsForRestart } from './restart-sessions';

function tab(overrides: Partial<TerminalTab>): TerminalTab {
	return {
		id: overrides.id ?? 'tab-1',
		title: overrides.title ?? 'shell',
		spec: { cwd: '/', cmd: ['bash'] },
		claudeSessionId: null,
		ptyId: null,
		mode: 'ephemeral',
		status: 'running',
		exitCode: null,
		createdAt: Date.now(),
		owner: { kind: 'sidepane' },
		...overrides,
	};
}

function setTabs(tabs: TerminalTab[]) {
	act(() => {
		useTerminalStore.setState({ tabs });
	});
}

describe('restart-sessions', () => {
	beforeEach(() => {
		setTabs([]);
	});

	it('counts only running, persistent tabs — an ephemeral or exited tab does not survive the restart', () => {
		setTabs([
			tab({ id: 'a', mode: 'persistent', status: 'running' }),
			tab({ id: 'b', mode: 'ephemeral', status: 'running' }),
			tab({ id: 'c', mode: 'persistent', status: 'exited' }),
			tab({ id: 'd', mode: 'persistent', status: 'spawning' }),
		]);

		const { result } = renderHook(() => useLiveSessionCount());
		expect(result.current).toBe(1);
	});

	it('marks a session resumable only when it has a captured claudeSessionId', () => {
		setTabs([
			tab({ id: 'claude-3', mode: 'persistent', status: 'running', claudeSessionId: 'sess_3', title: 'claude · session 3' }),
			tab({ id: 'nightly-pulse', mode: 'persistent', status: 'running', claudeSessionId: null, title: 'nightly-pulse' }),
		]);

		const { result } = renderHook(() => useLiveSessionsForRestart());
		expect(result.current).toEqual([
			{ id: 'claude-3', title: 'claude · session 3', resumable: true, persistent: true },
			{ id: 'nightly-pulse', title: 'nightly-pulse', resumable: false, persistent: true },
		]);
	});

	it('is empty when nothing is live', () => {
		const { result } = renderHook(() => useLiveSessionsForRestart());
		expect(result.current).toEqual([]);
		const { result: count } = renderHook(() => useLiveSessionCount());
		expect(count.current).toBe(0);
	});
});
