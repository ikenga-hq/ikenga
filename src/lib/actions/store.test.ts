// WP-52 effective-model store — unit tests (written under DEC-50, not run by
// WP-52; WP-63 runs them). Watcher-driven re-merge, package events, trust
// release (DEC-65) and the write / reset-override calls of G-ACTIONS-API.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	read: vi.fn(),
	watch: vi.fn(),
	writeActions: vi.fn(),
	writeKeybindings: vi.fn(),
	listen: vi.fn(),
	pkgStatus: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/tauri-cmd')>()),
	listen: h.listen,
	pkgKernelStatus: h.pkgStatus,
}));

vi.mock('./client', async (importOriginal) => ({
	...(await importOriginal<typeof import('./client')>()),
	readActionsFiles: h.read,
	watchActionsFiles: h.watch,
	writeActionsFile: h.writeActions,
	writeKeybindingsFile: h.writeKeybindings,
}));

import { getKeymap } from '@/lib/keymap/registry';
import { useShellStore } from '@/lib/shell/shell-store';
import {
	ActionsFileNotWritableError,
	bindingsFor,
	getEffectiveMenu,
	getEffectiveModel,
	hideAction,
	keyHolder,
	LockedActionError,
	LowerScopeOverrideError,
	rebindKey,
	resetKeyOverride,
	saveUserAction,
	setMenuOverride,
	startActionsStore,
	stopActionsStore,
	subscribeEffectiveModel,
	unbindKey,
	useActionsStore,
} from './store';
import type {
	ActionsChangeEvent,
	ActionsDocument,
	ActionsFilesResult,
	ActionsFileState,
	ActionsScope,
	KeybindingRule,
	KeybindingsDocument,
	TrustState,
} from './types';

function fileState<D extends ActionsDocument | KeybindingsDocument>(
	kind: 'actions' | 'keybindings',
	scope: ActionsScope,
	document: D | null,
	stale = false
): ActionsFileState<D> {
	return {
		kind,
		scope,
		path: `/${scope}/.ikenga/${kind}.json`,
		present: document != null,
		document,
		stale,
		validation: { errors: [], warnings: [] },
		error: null,
	};
}

function files(opts: {
	personalActions?: ActionsDocument;
	personalBindings?: KeybindingRule[];
	projectBindings?: KeybindingRule[];
	projectTrust?: TrustState;
	personalActionsStale?: boolean;
}): ActionsFilesResult {
	const kb = (bindings?: KeybindingRule[]) => (bindings ? { version: 1 as const, bindings } : null);
	return {
		personal: {
			scope: 'personal',
			actions: fileState('actions', 'personal', opts.personalActions ?? null, opts.personalActionsStale),
			keybindings: fileState('keybindings', 'personal', kb(opts.personalBindings)),
		},
		project: {
			scope: 'project',
			actions: fileState('actions', 'project', null),
			keybindings: fileState('keybindings', 'project', kb(opts.projectBindings)),
		},
		projectId: 'p1',
		projectRoot: '/work/p1',
		projectKeybindingsTrust: opts.projectBindings
			? { hash: 'h', ruleCount: opts.projectBindings.length, state: opts.projectTrust ?? 'untrusted' }
			: { hash: null, ruleCount: 0, state: 'absent' },
		trustError: null,
	};
}

type WatchCallback = (events: ActionsChangeEvent[]) => void | Promise<void>;
let watchCallback: WatchCallback | null = null;
const listeners = new Map<string, (event: { event: string; payload: unknown }) => void>();

function boundTo(command: string): string[] {
	return getKeymap()
		.filter((e) => e.command === command)
		.map((e) => e.key);
}

beforeEach(() => {
	watchCallback = null;
	listeners.clear();
	h.watch.mockImplementation((cb: WatchCallback) => {
		watchCallback = cb;
		return Promise.resolve(() => {});
	});
	h.listen.mockImplementation((event: string, handler: (event: { event: string; payload: unknown }) => void) => {
		listeners.set(event, handler);
		return Promise.resolve(() => {});
	});
	h.pkgStatus.mockResolvedValue({ installed: [], registries: {}, api_version: 5 });
	h.writeActions.mockResolvedValue({ written: true });
	h.writeKeybindings.mockResolvedValue({ written: true });
});

afterEach(() => {
	stopActionsStore();
	for (const mock of Object.values(h)) mock.mockReset();
});

describe('effective-model store', () => {
	it('publishes the merge to getKeymap() and re-merges on a watcher event, without a restart', async () => {
		h.read.mockResolvedValueOnce(files({}));
		await startActionsStore();
		expect(useActionsStore.getState().status).toBe('ready');
		expect(boundTo('pane.close')).toEqual(['mod+w']);

		const seen: number[] = [];
		const unsubscribe = subscribeEffectiveModel((model) => seen.push(model.keymap.negatives.length));

		h.read.mockResolvedValueOnce(files({ personalBindings: [{ key: 'mod+w', command: '-pane.close' }] }));
		expect(watchCallback).not.toBeNull();
		await watchCallback?.([
			{ path: '/personal/.ikenga/keybindings.json', file: 'keybindings', scope: 'personal' },
		]);

		await vi.waitFor(() => expect(boundTo('pane.close')).toEqual([]));
		expect(seen).toEqual([1]);
		unsubscribe();
	});

	it('re-merges the package layer on pkg-installed / pkg-uninstalled', async () => {
		h.read.mockResolvedValue(files({}));
		await startActionsStore();
		expect(getEffectiveModel().actionById.has('com.x:a')).toBe(false);

		h.pkgStatus.mockResolvedValue({
			installed: [{ id: 'com.x', installed_at: 1 }],
			registries: {
				context_actions: {
					count: 1,
					entries: [
						{
							pkg_id: 'com.x',
							qualified_id: 'com.x:a',
							id: 'a',
							label: 'A',
							when: { kind: 'session' },
							run: { kind: 'view', route: '/settings' },
							key: 'mod+shift+y',
						},
					],
				},
			},
			api_version: 5,
		});
		listeners.get('pkg-installed')?.({ event: 'pkg-installed', payload: { pkg_id: 'com.x' } });
		await vi.waitFor(() => expect(getEffectiveModel().actionById.has('com.x:a')).toBe(true));
		expect(boundTo('com.x:a')).toEqual(['mod+shift+y']);

		h.pkgStatus.mockResolvedValue({ installed: [], registries: {}, api_version: 5 });
		listeners.get('pkg-uninstalled')?.({ event: 'pkg-uninstalled', payload: { pkg_id: 'com.x' } });
		await vi.waitFor(() => expect(getEffectiveModel().actionById.has('com.x:a')).toBe(false));
	});

	it('releases held project keybindings on a trust event (DEC-65)', async () => {
		const projectBindings: KeybindingRule[] = [{ key: 'mod+w', command: '-pane.close' }];
		h.read.mockResolvedValueOnce(files({ projectBindings, projectTrust: 'untrusted' }));
		await startActionsStore();
		expect(getEffectiveModel().keymap.held).toHaveLength(1);
		expect(boundTo('pane.close')).toEqual(['mod+w']);

		h.read.mockResolvedValueOnce(files({ projectBindings, projectTrust: 'trusted' }));
		await watchCallback?.([
			{ path: '/work/p1/.ikenga/keybindings.json', file: 'keybindings', scope: 'project', reason: 'trust' },
		]);
		await vi.waitFor(() => expect(getEffectiveModel().keymap.held).toHaveLength(0));
		expect(boundTo('pane.close')).toEqual([]);
	});

	it('stopActionsStore() reverts getKeymap() to the defaults', async () => {
		h.read.mockResolvedValueOnce(files({ personalBindings: [{ key: 'mod+w', command: '-pane.close' }] }));
		await startActionsStore();
		expect(boundTo('pane.close')).toEqual([]);
		stopActionsStore();
		expect(boundTo('pane.close')).toEqual(['mod+w']);
	});
});

describe('writes', () => {
	it('hideAction refuses a locked item without writing (the merge rejects it too)', async () => {
		h.read.mockResolvedValue(files({}));
		await startActionsStore();
		await expect(hideAction('personal', 'delete')).rejects.toBeInstanceOf(LockedActionError);
		await expect(setMenuOverride('personal', 'files', { hidden: ['tab.close'] })).rejects.toBeInstanceOf(
			LockedActionError
		);
		expect(h.writeActions).not.toHaveBeenCalled();
	});

	it('hideAction writes `hidden` into every menu containing the id and never touches keys (DEC-58)', async () => {
		h.read.mockResolvedValue(files({}));
		await startActionsStore();
		await hideAction('personal', 'pane.split-right');
		expect(h.writeKeybindings).not.toHaveBeenCalled();
		const [scope, doc] = h.writeActions.mock.calls[0] as [ActionsScope, ActionsDocument];
		expect(scope).toBe('personal');
		expect(Object.keys(doc.menus ?? {}).sort()).toEqual(['native/view', 'palette', 'pane']);
		for (const override of Object.values(doc.menus ?? {})) expect(override.hidden).toEqual(['pane.split-right']);
	});

	it('unbindKey on a default appends one negative rule; rebindKey on an own rule edits it in place', async () => {
		h.read.mockResolvedValue(
			files({ personalActions: { version: 1 }, personalBindings: [{ key: 'mod+shift+e', command: 'tab.close' }] })
		);
		await startActionsStore();

		const def = getKeymap().find((e) => e.command === 'pane.close' && e.source === 'default');
		expect(def).toBeDefined();
		if (!def) return;
		await unbindKey('personal', def);
		const [, unbound] = h.writeKeybindings.mock.calls[0] as [ActionsScope, KeybindingsDocument];
		expect(unbound.bindings).toEqual([
			{ key: 'mod+shift+e', command: 'tab.close' },
			{ key: 'mod+w', command: '-pane.close', when: '!inputFocus' },
		]);

		const own = getKeymap().find((e) => e.command === 'tab.close' && e.source === 'personal');
		expect(own?.origin).toEqual({ scope: 'personal', index: 0 });
		if (!own) return;
		await rebindKey('personal', own, 'mod+alt+w');
		const [, rebound] = h.writeKeybindings.mock.calls[1] as [ActionsScope, KeybindingsDocument];
		expect(rebound.bindings).toEqual([{ key: 'mod+alt+w', command: 'tab.close' }]);
	});

	it('rebindKey on a default writes a negative + a positive rule; resetKeyOverride removes both', async () => {
		h.read.mockResolvedValue(files({}));
		await startActionsStore();
		const def = getKeymap().find((e) => e.command === 'pane.close');
		if (!def) throw new Error('pane.close missing');
		await rebindKey('personal', def, 'mod+alt+w');
		const [, doc] = h.writeKeybindings.mock.calls[0] as [ActionsScope, KeybindingsDocument];
		expect(doc).toMatchObject({
			$schema: 'urn:ikenga:keybindings:v1',
			version: 1,
			bindings: [
				{ key: 'mod+w', command: '-pane.close', when: '!inputFocus' },
				{ key: 'mod+alt+w', command: 'pane.close', when: '!inputFocus' },
			],
		});

		// Reset removes both override rules (and nothing else) at that scope.
		stopActionsStore();
		h.read.mockResolvedValue(
			files({ personalBindings: [...(doc.bindings ?? []), { key: 'mod+shift+j', command: 'companion.toggle' }] })
		);
		await startActionsStore();
		expect(boundTo('pane.close')).toEqual(['mod+alt+w']);
		await resetKeyOverride('personal', 'pane.close');
		const [, reset] = h.writeKeybindings.mock.calls[1] as [ActionsScope, KeybindingsDocument];
		expect(reset.bindings).toEqual([{ key: 'mod+shift+j', command: 'companion.toggle' }]);
	});

	it('refuses to overwrite a file that is malformed on disk', async () => {
		h.read.mockResolvedValue(files({ personalActions: { version: 1 }, personalActionsStale: true }));
		await startActionsStore();
		await expect(hideAction('personal', 'copy-name')).rejects.toBeInstanceOf(ActionsFileNotWritableError);
		expect(h.writeActions).not.toHaveBeenCalled();
	});

	it('refuses a file that is present but invalid with no valid copy (document null + validation error, §1.1)', async () => {
		const malformed = files({});
		malformed.personal.actions = {
			...malformed.personal.actions,
			present: true,
			document: null,
			stale: false,
			error: null,
			validation: { errors: [{ code: 'E_JSON', path: '', message: 'unexpected token' }], warnings: [] },
		};
		h.read.mockResolvedValue(malformed);
		await startActionsStore();
		await expect(hideAction('personal', 'copy-name')).rejects.toBeInstanceOf(ActionsFileNotWritableError);
		expect(h.writeActions).not.toHaveBeenCalled();
	});

	it('serializes overlapping writes, each from a fresh read: two concurrent edits both land', async () => {
		let disk: ActionsDocument | null = null;
		h.read.mockImplementation(() => Promise.resolve(files({ personalActions: disk ?? undefined })));
		h.writeActions.mockImplementation((_scope: ActionsScope, doc: ActionsDocument) => {
			disk = JSON.parse(JSON.stringify(doc)) as ActionsDocument;
			return Promise.resolve({ written: true });
		});
		await startActionsStore();
		const a = { id: 'a-one', name: 'A', scope: 'personal' as const, run: { kind: 'open' as const, url: '/a' } };
		const b = { id: 'b-two', name: 'B', scope: 'personal' as const, run: { kind: 'open' as const, url: '/b' } };
		await Promise.all([saveUserAction('personal', a), saveUserAction('personal', b)]);
		expect(h.writeActions).toHaveBeenCalledTimes(2);
		const [, last] = h.writeActions.mock.calls[1] as [ActionsScope, ActionsDocument];
		expect(last.actions?.map((x) => x.id)).toEqual(['a-one', 'b-two']);
	});

	it('an in-place rebind keeps the rule as written: no `platform` from a narrowed entry', async () => {
		h.read.mockResolvedValue(
			files({
				personalBindings: [
					{ key: 'mod+shift+e', command: 'tab.close' },
					{ key: 'mod+shift+e', command: '-tab.close', platform: 'other' },
				],
			})
		);
		await startActionsStore();
		const own = getKeymap().find((e) => e.command === 'tab.close' && e.source === 'personal');
		expect(own?.platformOnly).toBe('mac');
		if (!own) return;
		await rebindKey('personal', own, 'mod+alt+e');
		const [, doc] = h.writeKeybindings.mock.calls[0] as [ActionsScope, KeybindingsDocument];
		expect(doc.bindings?.[0]).toEqual({ key: 'mod+alt+e', command: 'tab.close' });
	});

	it('rebind / unbind below the entry layer throws LowerScopeOverrideError and writes nothing', async () => {
		h.read.mockResolvedValue(
			files({ projectBindings: [{ key: 'mod+shift+e', command: 'tab.close' }], projectTrust: 'trusted' })
		);
		await startActionsStore();
		const fromProject = getKeymap().find((e) => e.command === 'tab.close' && e.source === 'project');
		if (!fromProject) throw new Error('project rule missing');
		await expect(unbindKey('personal', fromProject)).rejects.toBeInstanceOf(LowerScopeOverrideError);
		await expect(rebindKey('personal', fromProject, 'mod+alt+e')).rejects.toBeInstanceOf(LowerScopeOverrideError);
		expect(h.writeKeybindings).not.toHaveBeenCalled();
	});

	it('the hidden item leaves the live menu after the re-read', async () => {
		h.read.mockResolvedValueOnce(files({}));
		await startActionsStore();
		expect(getEffectiveMenu('files')?.items.some((i) => i.kind === 'action' && i.id === 'copy-name')).toBe(true);
		h.read.mockResolvedValue(files({ personalActions: { version: 1, menus: { files: { hidden: ['copy-name'] } } } }));
		await hideAction('personal', 'copy-name');
		await vi.waitFor(() =>
			expect(getEffectiveMenu('files')?.items.some((i) => i.kind === 'action' && i.id === 'copy-name')).toBe(false)
		);
	});
});

describe('queries and project change', () => {
	it('bindingsFor / keyHolder read the current model', async () => {
		h.read.mockResolvedValue(files({ personalBindings: [{ key: 'mod+shift+e', command: 'tab.close' }] }));
		await startActionsStore();
		expect(bindingsFor('tab.close', 'other').map((e) => e.key)).toContain('mod+shift+e');
		expect(keyHolder('mod+shift+e', 'other')).toEqual({ kind: 'binding', command: 'tab.close', layer: 'personal' });
		expect(keyHolder('mod+c', 'other')).toEqual({ kind: 'native-role', key: 'mod+c' });
		expect(keyHolder('mod+alt+shift+f12', 'other')).toBeNull();
	});

	it('a project change re-reads files and packages for that project id', async () => {
		h.read.mockResolvedValue(files({}));
		await startActionsStore();
		const pkgCalls = h.pkgStatus.mock.calls.length;
		useShellStore.setState({ activeProject: { id: 'p2', root_path: '/work/p2', extra_roots: [] } });
		await vi.waitFor(() => expect(h.read).toHaveBeenLastCalledWith('p2'));
		expect(h.pkgStatus.mock.calls.length).toBeGreaterThan(pkgCalls);
	});
});
