// C1 — resolveTarget(target).send(text, context): PTY inject for a live
// terminal, chiRun for a `new` / `persistent` target, chiResume for a
// headless session (spec §5.3).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const ptyWrite = vi.fn(async () => {});
const chiRun = vi.fn(async () => ({ run_id: 'run-1', status: 'queued' }));
const chiResume = vi.fn(async () => ({ run_id: 'run-9', status: 'running' }));
vi.mock('@/lib/tauri-cmd', () => ({
	ptyWrite: (...a: unknown[]) => ptyWrite(...(a as [])),
	chiRun: (...a: unknown[]) => chiRun(...(a as [])),
	chiResume: (...a: unknown[]) => chiResume(...(a as [])),
}));

const ptys = new Map<string, { id: string; mode: 'ephemeral' | 'persistent'; exited: boolean }>();
vi.mock('@/terminal/pty-registry', () => ({
	getPty: (id: string) => ptys.get(id),
}));

const terminalTabs: Array<Record<string, unknown>> = [];
vi.mock('@/terminal/session-store', () => ({
	useTerminalStore: { getState: () => ({ tabs: terminalTabs }) },
}));

const shellState = {
	defaultEngineId: 'claude-code' as string | null,
	activeProject: { id: 'p1', root_path: '/work/royalti-co', extra_roots: [] },
};
vi.mock('@/lib/shell/shell-store', () => ({
	useShellStore: { getState: () => shellState },
}));

vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: {
		getState: () => ({
			focusedId: 'leaf-1',
			root: {
				type: 'leaf',
				id: 'leaf-1',
				activeTabIdx: 0,
				tabs: [{ kind: 'route', path: '/files' }],
			},
		}),
	},
}));

import {
	NO_ENGINE_REASON,
	contextCommentLine,
	currentDispatchContext,
	resolveTarget,
} from './resolve-target';

const ctx = { project: '/work/royalti-co', focusedView: '/files' };

beforeEach(() => {
	ptys.clear();
	terminalTabs.length = 0;
	shellState.defaultEngineId = 'claude-code';
	ptyWrite.mockClear();
	chiRun.mockClear();
	chiResume.mockClear();
});

afterEach(() => {
	vi.clearAllMocks();
});

describe('resolveTarget — live terminal (PTY inject)', () => {
	it('writes the text into the PTY with the context prepended as a comment line', async () => {
		ptys.set('term-a', { id: 'pty-7', mode: 'ephemeral', exited: false });
		terminalTabs.push({ id: 'term-a', spec: { cwd: '/', cmd: ['bash'] }, status: 'running' });

		const r = resolveTarget({ kind: 'session', session_id: 'term-a' });
		expect(r.kind).toBe('pty');
		await r.send('git status', ctx);

		expect(ptyWrite).toHaveBeenCalledTimes(1);
		expect(ptyWrite).toHaveBeenCalledWith(
			'pty-7',
			'# ikenga · project /work/royalti-co · view /files\rgit status\r'
		);
		expect(chiRun).not.toHaveBeenCalled();
		expect(chiResume).not.toHaveBeenCalled();
	});

	it('writes just the text when there is no context', async () => {
		ptys.set('term-a', { id: 'pty-7', mode: 'ephemeral', exited: false });
		await resolveTarget({ kind: 'session', session_id: 'term-a' }).send('ls');
		expect(ptyWrite).toHaveBeenCalledWith('pty-7', 'ls\r');
	});

	it('does not type a comment line into an agent TUI (a `wrap` terminal)', async () => {
		ptys.set('term-c', { id: 'pty-3', mode: 'ephemeral', exited: false });
		terminalTabs.push({
			id: 'term-c',
			spec: { cwd: '/', cmd: ['claude'], wrap: { engine: 'claude' } },
			status: 'running',
		});
		await resolveTarget({ kind: 'session', session_id: 'term-c' }).send('fix the test', ctx);
		expect(ptyWrite).toHaveBeenCalledWith('pty-3', 'fix the test\r');
	});

	it('falls back to the session store pty id when the registry has none', async () => {
		terminalTabs.push({
			id: 'term-b',
			ptyId: 'pty-9',
			status: 'running',
			spec: { cwd: '/', cmd: [] },
		});
		const r = resolveTarget({ kind: 'session', session_id: 'term-b' });
		expect(r.kind).toBe('pty');
		await r.send('pwd');
		expect(ptyWrite).toHaveBeenCalledWith('pty-9', 'pwd\r');
	});

	it('flattens newlines so the comment can never become a second command', () => {
		expect(contextCommentLine({ selection: 'a\nrm -rf /' })).toBe(
			'# ikenga · selection a rm -rf /'
		);
	});
});

describe('resolveTarget — Chi runtime', () => {
	it("a 'new' target calls chiRun with engine, cwd and the prompt (context appended)", async () => {
		const r = resolveTarget({ kind: 'new', engine_id: 'codex' });
		expect(r.kind).toBe('chi-run');
		await r.send('summarise the diff', ctx);
		expect(chiRun).toHaveBeenCalledWith({
			engineId: 'codex',
			prompt: 'summarise the diff\n\nContext: project /work/royalti-co; view /files',
			cwd: '/work/royalti-co',
			persistent: false,
		});
		expect(ptyWrite).not.toHaveBeenCalled();
	});

	it("a 'new' target with engine_id null uses defaultEngineId at send time", async () => {
		await resolveTarget({ kind: 'new', engine_id: null }).send('hi');
		expect(chiRun).toHaveBeenCalledWith(
			expect.objectContaining({ engineId: 'claude-code', prompt: 'hi', persistent: false })
		);
	});

	it("a 'persistent' target starts a persistent run", async () => {
		await resolveTarget({ kind: 'persistent', engine_id: null }).send('watch the build');
		expect(chiRun).toHaveBeenCalledWith(
			expect.objectContaining({ engineId: 'claude-code', persistent: true })
		);
	});

	it('a headless session (no live PTY) calls chiResume with the run id', async () => {
		const r = resolveTarget({ kind: 'session', session_id: 'run-9' });
		expect(r.kind).toBe('chi-resume');
		await r.send('and now the tests', ctx);
		expect(chiResume).toHaveBeenCalledWith(
			'run-9',
			'and now the tests\n\nContext: project /work/royalti-co; view /files'
		);
		expect(ptyWrite).not.toHaveBeenCalled();
	});

	it('an exited PTY no longer counts as live', async () => {
		ptys.set('term-x', { id: 'pty-1', mode: 'ephemeral', exited: true });
		expect(resolveTarget({ kind: 'session', session_id: 'term-x' }).kind).toBe('chi-resume');
	});

	it('nothing resolves without an engine: disabled with the Ngwa reason', async () => {
		shellState.defaultEngineId = null;
		const r = resolveTarget({ kind: 'new', engine_id: null });
		expect(r.kind).toBe('none');
		expect(r.disabledReason).toBe(NO_ENGINE_REASON);
		await expect(r.send('x')).rejects.toThrow(NO_ENGINE_REASON);
		expect(chiRun).not.toHaveBeenCalled();
	});

	it('send resolves to undefined — there is no response to render (ADR-021)', async () => {
		await expect(
			resolveTarget({ kind: 'new', engine_id: 'x' }).send('go')
		).resolves.toBeUndefined();
	});
});

describe('currentDispatchContext', () => {
	it('carries the active project and the focused view', () => {
		expect(currentDispatchContext()).toEqual({
			project: '/work/royalti-co',
			focusedView: '/files',
			selection: null,
		});
	});
});
