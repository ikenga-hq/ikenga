import { afterEach, describe, expect, it, vi } from 'vitest';

const { invokeMock, listenMock } = vi.hoisted(() => ({
	invokeMock: vi.fn(),
	listenMock: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	invoke: invokeMock,
	listen: listenMock,
}));

import {
	ActionsValidationError,
	actionsTrustGrant,
	actionsTrustRevoke,
	projectKeybindingsHeld,
	resolveUserActions,
	watchActionsFiles,
	writeKeybindingsFile,
	type ActionsFileState,
	type ActionsFilesResult,
	type ActionsScope,
	type ActionsDocument,
	type KeybindingsDocument,
	type UserAction,
} from './client';

function fileState<D extends ActionsDocument | KeybindingsDocument>(
	kind: 'actions' | 'keybindings',
	scope: ActionsScope,
	document: D | null
): ActionsFileState<D> {
	return {
		kind,
		scope,
		path: `/${scope}/.ikenga/${kind}.json`,
		present: document != null,
		document,
		stale: false,
		validation: { errors: [], warnings: [] },
		error: null,
	};
}

function action(id: string, scope: ActionsScope): UserAction {
	return { id, name: id, scope, run: { kind: 'open', url: `/${scope}` } };
}

describe('actions client', () => {
	afterEach(() => {
		vi.useRealTimers();
		invokeMock.mockReset();
		listenMock.mockReset();
	});

	it('throws the validation result when a write is refused', async () => {
		invokeMock.mockResolvedValue({
			written: false,
			kind: 'keybindings',
			scope: 'project',
			path: '/p/.ikenga/keybindings.json',
			validation: {
				errors: [{ code: 'E_OS_LAYER', path: '/bindings/0/scope', message: 'personal only' }],
				warnings: [],
			},
		});
		const write = writeKeybindingsFile('project', {
			version: 1,
			bindings: [{ key: 'alt+space', command: 'os.summon', scope: 'os' }],
		});
		await expect(write).rejects.toBeInstanceOf(ActionsValidationError);
		await expect(write).rejects.toMatchObject({
			validation: { errors: [{ code: 'E_OS_LAYER' }] },
		});
		expect(invokeMock).toHaveBeenCalledWith('keybindings_write', {
			scope: 'project',
			document: expect.any(Object),
			projectId: null,
		});
	});

	it('sends trust grants and revokes with explicit nulls', async () => {
		invokeMock.mockResolvedValue({});
		await actionsTrustGrant({ keybindings: 'abc' }, 'p1');
		expect(invokeMock).toHaveBeenLastCalledWith('actions_trust_grant', {
			request: { actions: [], keybindings: 'abc' },
			projectId: 'p1',
		});
		await actionsTrustRevoke();
		expect(invokeMock).toHaveBeenLastCalledWith('actions_trust_revoke', {
			request: { actionIds: null, keybindings: null },
			projectId: null,
		});
	});

	it('resolves project actions over personal ones by id', () => {
		const files: ActionsFilesResult = {
			personal: {
				scope: 'personal',
				actions: fileState<ActionsDocument>('actions', 'personal', {
					version: 1,
					actions: [action('explain-file', 'personal'), action('shared', 'personal')],
				}),
				keybindings: fileState<KeybindingsDocument>('keybindings', 'personal', null),
			},
			project: {
				scope: 'project',
				actions: fileState<ActionsDocument>('actions', 'project', {
					version: 1,
					actions: [action('shared', 'project')],
				}),
				keybindings: fileState<KeybindingsDocument>('keybindings', 'project', null),
			},
			projectId: 'p1',
			projectRoot: '/p',
			projectKeybindingsTrust: { hash: null, ruleCount: 0, state: 'absent' },
			trustError: null,
		};
		expect(
			resolveUserActions(files).map((entry) => [entry.action.id, entry.scope, entry.overriddenBy])
		).toEqual([
			['explain-file', 'personal', undefined],
			['shared', 'personal', 'project'],
			['shared', 'project', undefined],
		]);
	});

	it('holds project keybindings only while untrusted or changed', () => {
		expect(projectKeybindingsHeld({ hash: 'h', ruleCount: 1, state: 'untrusted' })).toBe(true);
		expect(projectKeybindingsHeld({ hash: 'h', ruleCount: 1, state: 'changed' })).toBe(true);
		expect(projectKeybindingsHeld({ hash: 'h', ruleCount: 1, state: 'trusted' })).toBe(false);
		expect(projectKeybindingsHeld({ hash: null, ruleCount: 0, state: 'absent' })).toBe(false);
		expect(projectKeybindingsHeld(null)).toBe(false);
	});

	it('coalesces change bursts and cancels on unsubscribe', async () => {
		vi.useFakeTimers();
		const unlisten = vi.fn();
		let handler: ((event: { payload: unknown }) => void) | undefined;
		listenMock.mockImplementation(async (_event: string, callback: typeof handler) => {
			handler = callback;
			return unlisten;
		});
		const onChange = vi.fn();
		const stop = await watchActionsFiles(onChange);
		expect(listenMock).toHaveBeenCalledWith('actions://changed', expect.any(Function));
		handler?.({ payload: { path: '/a', file: 'actions', scope: 'personal' } });
		handler?.({ payload: { path: '/k', file: 'keybindings', scope: 'project' } });
		vi.advanceTimersByTime(100);
		expect(onChange).toHaveBeenCalledOnce();
		expect(onChange.mock.calls[0][0]).toHaveLength(2);
		handler?.({ payload: { path: '/a', file: 'actions', scope: 'personal' } });
		stop();
		vi.advanceTimersByTime(100);
		expect(onChange).toHaveBeenCalledOnce();
		expect(unlisten).toHaveBeenCalledOnce();
	});
});
