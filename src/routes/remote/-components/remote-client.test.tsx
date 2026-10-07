// The phone dispatch bar sends only to agents (founder decision, 2026-10-07).
// It used to default to the first terminal and type `text + '\r'` into it — a
// plain bash terminal ran "Hello" as a shell command. Plain shells are no
// longer targets; a terminal is one only while an agent CLI is in front, and
// that is re-checked right before the write. Chi runs take a chi_resume.

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { AccessStatus } from '@/lib/access/client';
import type {
	ChiCacheRow,
	ChiRunResult,
	ForegroundProcess,
	TerminalDescriptor,
} from '@/lib/tauri-cmd';

const cmd = vi.hoisted(() => ({
	ptyWrite: vi.fn(async (_id: string, _data: string): Promise<void> => {}),
	ptyForeground: vi.fn(async (_id: string): Promise<ForegroundProcess | null> => null),
	chiResume: vi.fn(
		async (runId: string, _prompt: string): Promise<ChiRunResult> => ({
			run_id: runId,
			status: 'running',
		})
	),
}));

vi.mock('@/lib/tauri-cmd', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/tauri-cmd')>()),
	ptyWrite: cmd.ptyWrite,
	ptyForeground: cmd.ptyForeground,
	chiResume: cmd.chiResume,
}));

import { DispatchBar } from './remote-client';
import { NO_AGENT_TARGET, sessionRows } from './remote-model';

const status: AccessStatus = {
	tier: 't0',
	store: 'ok',
	principal: { principalId: 'p', username: 'ned', isAdmin: false },
	credential: { via: 'device', deviceId: 'd', tier: 'dispatch' },
	caps: ['files', 'sessions', 'dispatch'],
	adminStrength: false,
	publicUrl: null,
	sharingEnabled: false,
	share: null,
};

const fg = (name: string, args: string[] = [name]): ForegroundProcess => ({ pid: 9, name, args });

function term(
	ptyId: string,
	title: string,
	foreground: ForegroundProcess | null
): TerminalDescriptor {
	return {
		terminal_id: ptyId,
		pty_id: ptyId,
		title,
		label: null,
		cwd: '/w',
		argv: [title],
		status: 'running',
		pid: 1,
		foreground_command: foreground,
		owner_agent_id: null,
	};
}

const bash = term('pty-bash', 'bash', fg('bash'));
const claude = term('pty-claude', 'work', fg('claude'));
const run = {
	run_id: 'run-abcdef12',
	engine_id: 'codex',
	status: 'done',
	owner: 'ned',
	brief: 'nightly',
} as ChiCacheRow;

function renderBar(terms: TerminalDescriptor[], runs: ChiCacheRow[] = []) {
	return render(<DispatchBar status={status} sessions={sessionRows(terms, runs)} />);
}

const select = () => screen.getByRole('combobox', { name: 'Send to' }) as HTMLSelectElement;
const input = () => screen.getByRole('textbox', { name: 'Dispatch an instruction' });
const sendButton = () => screen.getByRole('button', { name: 'Send to session' });
const optionLabels = () => Array.from(select().options).map((o) => o.textContent);

beforeEach(() => {
	cmd.ptyWrite.mockClear();
	cmd.ptyForeground.mockReset();
	cmd.chiResume.mockClear();
});
afterEach(cleanup);

describe('DispatchBar — agents only', () => {
	it('never lists a plain bash terminal, and never writes to it', async () => {
		const user = userEvent.setup();
		renderBar([bash]);
		expect(optionLabels().some((l) => l?.includes('bash'))).toBe(false);
		await user.type(input(), 'Hello');
		expect((sendButton() as HTMLButtonElement).disabled).toBe(true);
		await user.keyboard('{Enter}');
		expect(cmd.ptyWrite).not.toHaveBeenCalled();
		expect(cmd.ptyForeground).not.toHaveBeenCalled();
	});

	it('shows the no-agent empty state when only shells are running', () => {
		renderBar([bash]);
		expect(screen.getByRole('status').textContent).toBe(NO_AGENT_TARGET);
		expect(select().disabled).toBe(true);
		expect((input() as HTMLInputElement).disabled).toBe(true);
	});

	it('has no default selection — Send stays disabled until a target is picked', async () => {
		const user = userEvent.setup();
		renderBar([bash, claude], [run]);
		expect(select().value).toBe('');
		await user.type(input(), 'Hello');
		expect((sendButton() as HTMLButtonElement).disabled).toBe(true);
		await user.keyboard('{Enter}');
		expect(cmd.ptyWrite).not.toHaveBeenCalled();
		expect(cmd.chiResume).not.toHaveBeenCalled();
	});

	it('lists an agent terminal and sends it the text + CR after re-checking the foreground', async () => {
		const user = userEvent.setup();
		cmd.ptyForeground.mockResolvedValue(fg('claude'));
		renderBar([bash, claude]);
		expect(optionLabels()).toContain('claude · work');
		await user.selectOptions(select(), 'pty:pty-claude');
		await user.type(input(), 'Hello');
		await user.click(sendButton());
		await waitFor(() => expect(cmd.ptyWrite).toHaveBeenCalledWith('pty-claude', 'Hello\r'));
		expect(cmd.ptyForeground).toHaveBeenCalledWith('pty-claude');
		expect(cmd.ptyWrite).toHaveBeenCalledTimes(1);
		// The selection does not stick: the next send needs a fresh pick.
		expect(select().value).toBe('');
	});

	it('refuses, without writing, when the foreground turned into bash after selection', async () => {
		const user = userEvent.setup();
		cmd.ptyForeground.mockResolvedValue(fg('bash'));
		renderBar([claude]);
		await user.selectOptions(select(), 'pty:pty-claude');
		await user.type(input(), 'rm -rf build');
		await user.click(sendButton());
		await waitFor(() =>
			expect(screen.getByRole('status').textContent).toMatch(/no longer running an agent/)
		);
		expect(cmd.ptyWrite).not.toHaveBeenCalled();
		expect(select().value).toBe('');
	});

	it('refuses when the foreground is gone at send time', async () => {
		const user = userEvent.setup();
		cmd.ptyForeground.mockResolvedValue(null);
		renderBar([claude]);
		await user.selectOptions(select(), 'pty:pty-claude');
		await user.type(input(), 'Hello');
		await user.click(sendButton());
		await waitFor(() =>
			expect(screen.getByRole('status').textContent).toMatch(/no longer running an agent/)
		);
		expect(cmd.ptyWrite).not.toHaveBeenCalled();
	});

	it('sends to a Chi run with chi_resume and the text as the follow-up prompt', async () => {
		const user = userEvent.setup();
		renderBar([bash], [run]);
		expect(optionLabels()).toContain('Chi · codex · nightly');
		await user.selectOptions(select(), 'run:run-abcdef12');
		await user.type(input(), 'Now add tests');
		await user.click(sendButton());
		await waitFor(() =>
			expect(cmd.chiResume).toHaveBeenCalledWith('run-abcdef12', 'Now add tests')
		);
		expect(cmd.ptyWrite).not.toHaveBeenCalled();
		expect(cmd.ptyForeground).not.toHaveBeenCalled();
	});
});
