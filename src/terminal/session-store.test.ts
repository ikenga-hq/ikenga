import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ptyTerminalList, type TerminalDescriptor } from '@/lib/tauri-cmd';
import { stripSecretEnv, useTerminalStore } from './session-store';

// The store loads the SQL shim lazily on persist; mocking it
// keeps the tests offline. Failure to load falls back to localStorage
// (also fine in jsdom).
vi.mock('@/lib/transport/sql-shim', () => ({
	default: {
		load: async () => {
			throw new Error('sql disabled in tests');
		},
	},
}));

// `rehydrateFromDb` now reconciles against live PTYs.
vi.mock('@/lib/tauri-cmd', () => ({
	ptyTerminalList: vi.fn().mockResolvedValue([]),
}));

function reset() {
	useTerminalStore.setState({ tabs: [], activeId: null, rehydrated: false });
}

describe('useTerminalStore ownership', () => {
	beforeEach(reset);

	it('add() defaults owner to sidepane', () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id)!;
		expect(tab.owner).toEqual({ kind: 'sidepane' });
	});

	it('attachToStudio transitions ownership to studio', () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		const res = useTerminalStore.getState().attachToStudio(id, 'pane-1', '/a.html');
		expect(res).toEqual({ ok: true });
		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id)!;
		expect(tab.owner).toEqual({ kind: 'studio', paneId: 'pane-1', artifactPath: '/a.html' });
	});

	it('re-attach from same pane succeeds idempotently', () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		useTerminalStore.getState().attachToStudio(id, 'pane-1', '/a.html');
		const res = useTerminalStore.getState().attachToStudio(id, 'pane-1', '/a.html');
		expect(res).toEqual({ ok: true });
	});

	it('re-attach from different pane returns conflict', () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		useTerminalStore.getState().attachToStudio(id, 'pane-1', '/a.html');
		const res = useTerminalStore.getState().attachToStudio(id, 'pane-2', '/b.html');
		expect(res).toEqual({ ok: false, requiresConfirm: true, previousPaneId: 'pane-1' });
		// Ownership must NOT have changed on a refused attach.
		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id)!;
		expect(tab.owner).toMatchObject({ kind: 'studio', paneId: 'pane-1' });
	});

	it('force-attach from different pane overrides', () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		useTerminalStore.getState().attachToStudio(id, 'pane-1', '/a.html');
		const res = useTerminalStore
			.getState()
			.attachToStudio(id, 'pane-2', '/b.html', { force: true });
		expect(res).toEqual({ ok: true });
		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id)!;
		expect(tab.owner).toEqual({ kind: 'studio', paneId: 'pane-2', artifactPath: '/b.html' });
	});

	it('detachFromStudio restores sidepane ownership', () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		useTerminalStore.getState().attachToStudio(id, 'pane-1', '/a.html');
		useTerminalStore.getState().detachFromStudio(id);
		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id)!;
		expect(tab.owner).toEqual({ kind: 'sidepane' });
	});

	it('detach on already-sidepane is a no-op', () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		useTerminalStore.getState().detachFromStudio(id);
		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id)!;
		expect(tab.owner).toEqual({ kind: 'sidepane' });
	});

	it('findStudioAttachment returns the right tab', () => {
		const a = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		const b = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['claude'] });
		useTerminalStore.getState().attachToStudio(b, 'pane-9', '/x.html');
		expect(useTerminalStore.getState().findStudioAttachment('pane-9')?.id).toBe(b);
		expect(useTerminalStore.getState().findStudioAttachment('pane-1')).toBeNull();
		// `a` was never attached.
		expect(useTerminalStore.getState().tabs.find((t) => t.id === a)!.owner.kind).toBe('sidepane');
	});
});

describe('rehydrateFromDb & auto-resume', () => {
	beforeEach(() => {
		reset();
		localStorage.clear();
	});

	it('restores previously running tabs with spawning status', async () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		useTerminalStore.getState().setStatus(id, 'running');

		// Force persist synchronously to localStorage
		await useTerminalStore.getState().persistToDb();

		// Reset in-memory state
		useTerminalStore.setState({ tabs: [], activeId: null, rehydrated: false });

		// Rehydrate from stored state
		await useTerminalStore.getState().rehydrateFromDb();

		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id);
		expect(tab).toBeDefined();
		expect(tab?.status).toBe('spawning');
		expect(tab?.wasRunning).toBe(true);
	});

	it('keeps manually exited tabs as exited on rehydrate', async () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		useTerminalStore.getState().setStatus(id, 'exited', 0);

		await useTerminalStore.getState().persistToDb();
		useTerminalStore.setState({ tabs: [], activeId: null, rehydrated: false });

		await useTerminalStore.getState().rehydrateFromDb();

		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id);
		expect(tab).toBeDefined();
		expect(tab?.status).toBe('exited');
		expect(tab?.wasRunning).toBe(false);
	});

	it('updateCwd updates the tab spec cwd', () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		useTerminalStore.getState().updateCwd(id, '/new/working/dir');
		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id);
		expect(tab?.spec.cwd).toBe('/new/working/dir');
	});

	it('reconciles live PTYs on rehydrate and marks them running', async () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		useTerminalStore.getState().setStatus(id, 'running');
		await useTerminalStore.getState().persistToDb();

		useTerminalStore.setState({ tabs: [], activeId: null, rehydrated: false });

		const live: TerminalDescriptor[] = [
			{
				terminal_id: id,
				pty_id: 'pty-123',
				title: 'bash',
				label: null,
				cwd: '/tmp',
				argv: ['bash'],
				status: 'running',
				pid: 42,
				foreground_command: null,
				owner_agent_id: null,
			},
		];
		vi.mocked(ptyTerminalList).mockResolvedValueOnce(live);

		await useTerminalStore.getState().rehydrateFromDb();

		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id);
		expect(tab?.status).toBe('running');
		expect(tab?.ptyId).toBe('pty-123');
		expect(tab?.wasRunning).toBe(true);
	});

	it('respawns unmatched running tabs when no live PTY matches', async () => {
		const id = useTerminalStore.getState().add({ cwd: '/tmp', cmd: ['bash'] });
		useTerminalStore.getState().setStatus(id, 'running');
		await useTerminalStore.getState().persistToDb();

		useTerminalStore.setState({ tabs: [], activeId: null, rehydrated: false });

		vi.mocked(ptyTerminalList).mockResolvedValueOnce([]);

		await useTerminalStore.getState().rehydrateFromDb();

		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id);
		expect(tab?.status).toBe('spawning');
		expect(tab?.ptyId).toBeNull();
	});

	it('persists and restores claudeSessionId + wrap opts for resume', async () => {
		const id = useTerminalStore.getState().add({
			cwd: '/tmp',
			cmd: ['claude'],
			wrap: { engine: 'claude', permissionMode: 'plan' },
		});
		useTerminalStore.getState().setClaudeSessionId(id, 'sess-789');
		useTerminalStore.getState().setStatus(id, 'running');

		await useTerminalStore.getState().persistToDb();
		useTerminalStore.setState({ tabs: [], activeId: null, rehydrated: false });

		vi.mocked(ptyTerminalList).mockResolvedValueOnce([]);
		await useTerminalStore.getState().rehydrateFromDb();

		const tab = useTerminalStore.getState().tabs.find((t) => t.id === id);
		expect(tab?.claudeSessionId).toBe('sess-789');
		expect(tab?.spec.wrap).toMatchObject({ engine: 'claude', permissionMode: 'plan' });
		expect(tab?.status).toBe('spawning');
	});
});

describe('stripSecretEnv (ADR-013 §Addendum Decision 3)', () => {
	it('drops credential-shaped keys', () => {
		const out = stripSecretEnv({
			ANTHROPIC_API_KEY: 'sk-ant-xxx',
			OPENAI_API_KEY: 'sk-xxx',
			GEMINI_API_KEY: 'g-xxx',
			GITHUB_TOKEN: 'ghp_xxx',
			AWS_SECRET_ACCESS_KEY: 'aws-xxx',
			DB_PASSWORD: 'hunter2',
			CLIENT_SECRET: 'cs-xxx',
			SESSION_TOKEN: 'st-xxx',
		});
		expect(out).toEqual({});
	});

	it('keeps non-secret keys', () => {
		const out = stripSecretEnv({
			PATH: '/usr/bin',
			TERM: 'xterm-256color',
			LANG: 'en_US.UTF-8',
			MY_FEATURE_FLAG: '1',
		});
		expect(out).toEqual({
			PATH: '/usr/bin',
			TERM: 'xterm-256color',
			LANG: 'en_US.UTF-8',
			MY_FEATURE_FLAG: '1',
		});
	});

	it('mixes: strips only the secret-shaped ones', () => {
		const out = stripSecretEnv({ PATH: '/usr/bin', NPM_TOKEN: 'npm-xxx', NODE_ENV: 'production' });
		expect(out).toEqual({ PATH: '/usr/bin', NODE_ENV: 'production' });
	});

	it('passes through undefined', () => {
		expect(stripSecretEnv(undefined)).toBeUndefined();
	});
});
