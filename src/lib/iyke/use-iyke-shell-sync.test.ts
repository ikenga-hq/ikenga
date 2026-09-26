// WP-21: `useIykeShellSync` pushes the store's derived `activeProject` into
// the Rust mirror behind `iyke state` (`shell.active_project`), and the keymap
// registry behind `GET /iyke/keys` exactly once when the workspace mounts.

import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { DEFAULT_KEYMAP } from '@/lib/keymap/defaults';
import { formatKeyLabel } from '@/lib/keymap/platform';
import { useShellStore } from '@/lib/shell/shell-store';
import type { IykeKeymapEntry } from '@/lib/tauri-cmd';

const iykeSetFrame = vi.fn((_args: unknown) => Promise.resolve());
const iykeSetActionsFrame = vi.fn((_args: unknown) => Promise.resolve());
const iykeActionsRequestDone = vi.fn((_requestId: string, _result: unknown) => Promise.resolve());
// `listen` backs the WP-62 round-trip listeners. Every test in this file
// mounts the hook, so it must resolve to a real (no-op) unlisten function —
// an unresolved promise would leave `useEffect`'s cleanup with nothing to
// call and dangle a rejected-promise warning across tests.
const listen = vi.fn((_event: string, _handler: (e: { event: string; payload: unknown }) => void) =>
	Promise.resolve(() => {})
);
const setShell = vi.fn((_args: unknown) => Promise.resolve());

vi.mock('@/lib/tauri-cmd', () => ({
	iykeSetFrame: (args: unknown) => iykeSetFrame(args),
	iykeSetActionsFrame: (args: unknown) => iykeSetActionsFrame(args),
	iykeActionsRequestDone: (requestId: string, result: unknown) => iykeActionsRequestDone(requestId, result),
	listen: (event: string, handler: (e: { event: string; payload: unknown }) => void) => listen(event, handler),
}));
vi.mock('./client', () => ({ setShell: (args: unknown) => setShell(args) }));

import {
	actionsMirrorPayload,
	keymapPayload,
	menusMirrorPayload,
	useIykeShellSync,
} from './use-iyke-shell-sync';
import type { EffectiveAction, EffectiveModel } from './keymap-bridge';

type FrameArgs = {
	activeProject?: { id: string; root_path: string | null; extra_roots: string[] } | null;
	keymap?: IykeKeymapEntry[] | null;
};

function frameCalls(): FrameArgs[] {
	return iykeSetFrame.mock.calls.map((c) => c[0] as FrameArgs);
}
const keymapPushes = () => frameCalls().filter((a) => a.keymap);
const projectPushes = () => frameCalls().filter((a) => a.activeProject);

describe('useIykeShellSync — WP-21 frame push', () => {
	beforeEach(() => {
		iykeSetFrame.mockClear();
		iykeSetActionsFrame.mockClear();
		iykeActionsRequestDone.mockClear();
		listen.mockClear();
		setShell.mockClear();
		useShellStore.setState({
			activeProjectId: 'default',
			projectExtraRoots: {},
			carriedRoots: [],
			activeProject: { id: 'default', root_path: null, extra_roots: [] },
		});
	});
	afterEach(() => {
		vi.restoreAllMocks();
	});

	it('pushes active_project matching the store, and re-pushes when it changes', () => {
		renderHook(() => useIykeShellSync());

		expect(projectPushes()).toHaveLength(1);
		expect(projectPushes()[0].activeProject).toEqual(useShellStore.getState().activeProject);
		expect(projectPushes()[0].activeProject).toEqual({
			id: 'default',
			root_path: null,
			extra_roots: [],
		});

		act(() => {
			useShellStore.getState().setProjectExtraRoots('default', ['/work/a', ' /work/b ', '/work/a']);
		});

		const store = useShellStore.getState().activeProject;
		expect(store.extra_roots).toEqual(['/work/a', '/work/b']);
		expect(projectPushes()).toHaveLength(2);
		expect(projectPushes()[1].activeProject).toEqual(store);
		// Pushes carry only the field they're about.
		expect(projectPushes()[1].keymap).toBeUndefined();

		act(() => {
			useShellStore.setState({
				activeProjectId: 'p1',
				activeProject: { id: 'p1', root_path: '/work/p1', extra_roots: ['/x'] },
			});
		});
		expect(projectPushes()).toHaveLength(3);
		expect(projectPushes()[2].activeProject).toEqual({
			id: 'p1',
			root_path: '/work/p1',
			extra_roots: ['/x'],
		});
	});

	it('does not re-push active_project on unrelated store changes', () => {
		renderHook(() => useIykeShellSync());
		const before = projectPushes().length;
		act(() => {
			useShellStore.setState({ sidebarCollapsed: !useShellStore.getState().sidebarCollapsed });
		});
		expect(projectPushes()).toHaveLength(before);
	});

	it('pushes the full keymap registry exactly once at mount', () => {
		const { rerender } = renderHook(() => useIykeShellSync());

		expect(keymapPushes()).toHaveLength(1);
		const pushed = keymapPushes()[0].keymap!;
		expect(pushed).toHaveLength(DEFAULT_KEYMAP.length);
		expect(pushed.map((e) => e.command)).toEqual(DEFAULT_KEYMAP.map((e) => e.command));
		for (const [i, e] of DEFAULT_KEYMAP.entries()) {
			expect(pushed[i]).toMatchObject({
				command: e.command,
				key: e.key,
				when: e.when,
				source: e.source,
				label: e.label,
				key_label: formatKeyLabel(e.key),
			});
			expect(pushed[i].platform_only).toBe(e.platformOnly);
		}
		// The keymap push carries no active_project.
		expect(keymapPushes()[0].activeProject).toBeUndefined();

		// Re-renders and store churn never re-push it.
		rerender();
		act(() => {
			useShellStore.getState().setProjectExtraRoots('default', ['/work/c']);
			useShellStore.setState({ sidebarCollapsed: !useShellStore.getState().sidebarCollapsed });
		});
		rerender();
		expect(keymapPushes()).toHaveLength(1);
	});

	it('keymapPayload omits platform_only when the entry has none', () => {
		const [row] = keymapPayload({
			entries: [{ command: 'x.y', key: 'mod+k', when: 'global', source: 'default', label: 'X' }],
			held: [],
		});
		expect(row).not.toHaveProperty('platform_only');
		expect(row).not.toHaveProperty('status');
		expect(row.key_label).toBe(formatKeyLabel('mod+k'));
	});

	it('keymapPayload (S3, DEC-65) projects held project rules with status "held" and their trust', () => {
		const [row] = keymapPayload({
			entries: [],
			held: [
				{
					index: 0,
					rule: { key: 'mod+shift+d', command: 'delete', when: 'filesFocus' },
					trust: 'untrusted',
				},
			],
		});
		expect(row).toMatchObject({
			command: 'delete',
			key: 'mod+shift+d',
			source: 'project',
			status: 'held',
			trust: 'untrusted',
		});
	});

	it('keymapPayload keeps the `-` of a held removal rule, so it never reads as a new binding', () => {
		const [row] = keymapPayload({
			entries: [],
			held: [{ index: 1, rule: { key: 'mod+w', command: '-pane.close' }, trust: 'changed' }],
		});
		expect(row.command).toBe('-pane.close');
		expect(row.label).toBe('pane.close');
		expect(row.status).toBe('held');
		expect(row.trust).toBe('changed');
	});

	it('logs, never throws, when the push fails', async () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		iykeSetFrame.mockImplementation(() => Promise.reject(new Error('boom')));
		renderHook(() => useIykeShellSync());
		await act(async () => {
			await Promise.resolve();
		});
		expect(warn.mock.calls.some((c) => String(c[0]).includes('set_frame'))).toBe(true);
		iykeSetFrame.mockImplementation(() => Promise.resolve());
	});

	it('pushes the effective actions/menus mirror once at mount and registers the four round-trip listeners', () => {
		renderHook(() => useIykeShellSync());

		expect(iykeSetActionsFrame).toHaveBeenCalledTimes(1);
		const call = iykeSetActionsFrame.mock.calls[0][0] as {
			actions: unknown[];
			menus: Record<string, unknown>;
		};
		// The store's `EMPTY_MODEL` (no files, no packages) still has a
		// built-in-only action set and a full menu map — never an empty push.
		expect(Array.isArray(call.actions)).toBe(true);
		expect(call.actions.length).toBeGreaterThan(0);
		expect(Object.keys(call.menus).length).toBeGreaterThan(0);

		const listenedEvents = listen.mock.calls.map((c) => c[0]);
		expect(listenedEvents).toEqual(
			expect.arrayContaining([
				'iyke://actions-set-request',
				'iyke://actions-import-request',
				'iyke://keys-set-request',
				'iyke://keys-resolve-request',
			])
		);
	});
});

describe('actionsMirrorPayload / menusMirrorPayload — WP-62 mirror projection', () => {
	function fakeAction(overrides: Partial<EffectiveAction> = {}): EffectiveAction {
		return {
			id: 'explain-file',
			name: 'Explain this file',
			description: 'Sends a prompt to the active session.',
			source: 'personal',
			run: { kind: 'chi', target: 'active', prompt: 'Explain {{file.path}}' },
			placements: [{ at: 'files', when: "resource =~ '*.ts'" }],
			locked: false,
			danger: false,
			hosted: false,
			osOnly: false,
			editable: true,
			...overrides,
		} as EffectiveAction;
	}

	it('flattens an EffectiveAction into the wire shape, omitting empty optionals', () => {
		const [row] = actionsMirrorPayload([fakeAction()]);
		expect(row).toEqual({
			id: 'explain-file',
			name: 'Explain this file',
			description: 'Sends a prompt to the active session.',
			source: 'personal',
			run_kind: 'chi',
			placements: ['files'],
			locked: false,
			hosted: false,
			danger: false,
		});
		expect(row).not.toHaveProperty('icon');
		expect(row).not.toHaveProperty('pkg_id');
	});

	it('carries icon and pkg_id through when present', () => {
		const [row] = actionsMirrorPayload([
			fakeAction({ icon: 'sparkles', pkgId: 'com.ikenga.git', source: 'package' }),
		]);
		expect(row.icon).toBe('sparkles');
		expect(row.pkg_id).toBe('com.ikenga.git');
		expect(row.source).toBe('package');
	});

	it('never adds trust_state for a non-project action', () => {
		const [row] = actionsMirrorPayload([fakeAction()]);
		expect(row).not.toHaveProperty('trust_state');
	});

	it('(S3, DEC-55) reports a project action\'s trust state from the map, fail-closed to "untrusted"', () => {
		const trust = new Map([['refresh-pulse', 'trusted' as const]]);
		const [trusted, unknownYet] = actionsMirrorPayload(
			[
				fakeAction({ id: 'refresh-pulse', source: 'project' }),
				fakeAction({ id: 'not-yet-scanned', source: 'project' }),
			],
			trust
		);
		expect(trusted.trust_state).toBe('trusted');
		expect(unknownYet.trust_state).toBe('untrusted');
	});

	it('projects every menu id the model reports, collapsing action items to {id, name, source}', () => {
		const model = {
			menus: {
				ids: ['files', 'empty-menu'],
				get: (id: string) => {
					if (id === 'files') {
						return {
							id: 'files',
							items: [
								{
									kind: 'action',
									id: 'open',
									action: fakeAction({ id: 'open', name: 'Open', source: 'builtin' }),
									layer: 'default',
								},
								{ kind: 'separator' },
								{
									kind: 'action',
									id: 'explain-file',
									action: fakeAction(),
									layer: 'personal',
									when: "resource =~ '*.ts'",
								},
							],
							hidden: ['copy-name'],
							overrides: {},
						};
					}
					if (id === 'empty-menu') {
						return { id: 'empty-menu', items: [], hidden: [], overrides: {} };
					}
					return null;
				},
			},
		} as unknown as EffectiveModel;

		const mirror = menusMirrorPayload(model);
		expect(Object.keys(mirror)).toEqual(['files', 'empty-menu']);
		expect(mirror.files.hidden).toEqual(['copy-name']);
		expect(mirror.files.items).toEqual([
			{ kind: 'action', id: 'open', name: 'Open', source: 'builtin' },
			{ kind: 'separator' },
			{
				kind: 'action',
				id: 'explain-file',
				name: 'Explain this file',
				source: 'personal',
				when: "resource =~ '*.ts'",
			},
		]);
		expect(mirror['empty-menu']).toEqual({ id: 'empty-menu', items: [], hidden: [] });
	});

	it('(S6) folds extraIds in even when menus.ids omits them, skipping ones .get() still can\'t resolve', () => {
		const model = {
			menus: {
				ids: ['files'],
				get: (id: string) => {
					if (id === 'files') return { id: 'files', items: [], hidden: [], overrides: {} };
					if (id === 'section/scratchpads') {
						return { id: 'section/scratchpads', items: [], hidden: [], overrides: {} };
					}
					return null;
				},
			},
		} as unknown as EffectiveModel;

		const mirror = menusMirrorPayload(model, ['section/scratchpads', 'section/uninstalled-pkg']);
		expect(Object.keys(mirror).sort()).toEqual(['files', 'section/scratchpads']);
	});
});
