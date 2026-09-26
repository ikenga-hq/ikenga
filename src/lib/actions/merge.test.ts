// WP-52 effective-model merge — unit tests (written under DEC-50, not run
// by WP-52; WP-63 runs them). G-ACTIONS §1.4, §1.5, §2, §7, §8.3, §9, §12.

import { describe, expect, it } from 'vitest';
import { DEFAULT_KEYMAP, type KeymapEntry } from '@/lib/keymap/defaults';
import { comparableKeySequence, entriesForPlatform, type KeymapPlatform } from '@/lib/keymap/registry';
import { buildEffectiveModel, isProjectKeybindingsHeld, type MergeInput, resolveKeypressWinner } from './merge';
import type { EffectiveMenu } from './menus';
import {
	deriveContextActionKeyWhen,
	type PackageActionSource,
	packagePlacements,
	readPackageActions,
} from './registry';
import type {
	ActionsDocument,
	ActionsFilesResult,
	ActionsFileState,
	ActionsScope,
	KeybindingRule,
	KeybindingsDocument,
	TrustState,
	UserAction,
} from './types';

// ─── Fixtures ────────────────────────────────────────────────────────────────

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

interface FilesOpts {
	personalActions?: ActionsDocument;
	personalBindings?: KeybindingRule[];
	projectActions?: ActionsDocument;
	projectBindings?: KeybindingRule[];
	projectTrust?: TrustState;
}

function makeFiles(opts: FilesOpts = {}): ActionsFilesResult {
	const kb = (bindings?: KeybindingRule[]): KeybindingsDocument | null =>
		bindings ? { version: 1, bindings } : null;
	return {
		personal: {
			scope: 'personal',
			actions: fileState('actions', 'personal', opts.personalActions ?? null),
			keybindings: fileState('keybindings', 'personal', kb(opts.personalBindings)),
		},
		project: {
			scope: 'project',
			actions: fileState('actions', 'project', opts.projectActions ?? null),
			keybindings: fileState('keybindings', 'project', kb(opts.projectBindings)),
		},
		projectId: 'p1',
		projectRoot: '/work/p1',
		projectKeybindingsTrust: opts.projectBindings
			? { hash: 'h', ruleCount: opts.projectBindings.length, state: opts.projectTrust ?? 'trusted' }
			: { hash: null, ruleCount: 0, state: 'absent' },
		trustError: null,
	};
}

function userAction(id: string, scope: ActionsScope, extra: Partial<UserAction> = {}): UserAction {
	return { id, name: `Action ${id}`, scope, run: { kind: 'open', url: `/${id}` }, ...extra };
}

function pkgAction(p: Partial<PackageActionSource> & Pick<PackageActionSource, 'pkgId' | 'localId'>): PackageActionSource {
	return {
		id: `${p.pkgId}:${p.localId}`,
		name: p.localId,
		origin: 'context_action',
		run: { kind: 'view', route: '/settings' },
		selector: { kind: 'file' },
		keyRequest: null,
		installedAt: 1,
		...p,
	};
}

function model(input: Partial<MergeInput> = {}) {
	return buildEffectiveModel({ files: null, packages: [], ...input });
}

/** Effective entries on `platform` bound to `key` (comparable form). */
function onKey(entries: readonly KeymapEntry[], key: string, platform: KeymapPlatform): KeymapEntry[] {
	const k = comparableKeySequence(key, platform);
	return entriesForPlatform(entries, platform).filter((e) => comparableKeySequence(e.key, platform) === k);
}

function menuIds(menu: EffectiveMenu | null): string[] {
	return (menu?.items ?? []).flatMap((item) => (item.kind === 'action' ? [item.id] : ['---']));
}

// ─── Merge order ─────────────────────────────────────────────────────────────

describe('merge order (DEC-59, §2.1)', () => {
	it('with no files and no packages the keymap is exactly the defaults', () => {
		const m = model();
		expect(m.keymap.entries).toEqual(DEFAULT_KEYMAP);
		expect(m.keymap.held).toEqual([]);
		expect(m.keymap.conflicts.mac.clashes).toEqual([]);
		expect(m.keymap.conflicts.other.clashes).toEqual([]);
	});

	it('orders entries default < package < personal < project', () => {
		const m = model({
			files: makeFiles({
				personalActions: { version: 1, actions: [userAction('mine', 'personal')] },
				personalBindings: [{ key: 'mod+shift+y', command: 'mine' }],
				projectActions: { version: 1, actions: [userAction('ours', 'project')] },
				projectBindings: [{ key: 'mod+shift+u', command: 'ours' }],
			}),
			packages: [pkgAction({ pkgId: 'com.x.git', localId: 'stage', keyRequest: 'mod+shift+g' })],
		});
		const sources = m.keymap.entries.map((e) => e.source);
		const firstOf = (s: string) => sources.indexOf(s as never);
		const lastOf = (s: string) => sources.lastIndexOf(s as never);
		expect(lastOf('default')).toBeLessThan(firstOf('package'));
		expect(lastOf('package')).toBeLessThan(firstOf('personal'));
		expect(lastOf('personal')).toBeLessThan(firstOf('project'));
		expect(m.actions.map((a) => a.source).lastIndexOf('builtin')).toBeLessThan(
			m.actions.findIndex((a) => a.source === 'package')
		);
	});

	it('a higher layer wins the same key and `when` at a keypress (§2.3), and the pair is reported as a clash', () => {
		const m = model({
			files: makeFiles({
				personalActions: { version: 1, actions: [userAction('mine', 'personal')] },
				personalBindings: [{ key: 'mod+b', command: 'mine' }],
				projectActions: { version: 1, actions: [userAction('ours', 'project')] },
				projectBindings: [{ key: 'mod+b', command: 'ours' }],
			}),
		});
		const candidates = onKey(m.keymap.entries, 'mod+b', 'mac');
		expect(candidates.map((e) => e.command)).toEqual(['explorer.toggle', 'mine', 'ours']);
		expect(resolveKeypressWinner(candidates, m.keymap.entries)?.command).toBe('ours');
		// explorer.toggle is `always`; the file rules default to `always` too.
		expect(m.keymap.conflicts.mac.clashes.length).toBeGreaterThan(0);
	});

	it('a project action with a personal id wins whole; the personal one is shadowed (§1.2)', () => {
		const m = model({
			files: makeFiles({
				personalActions: { version: 1, actions: [userAction('dup', 'personal', { name: 'Mine' })] },
				projectActions: { version: 1, actions: [userAction('dup', 'project', { name: 'Ours' })] },
			}),
		});
		expect(m.actionById.get('dup')?.name).toBe('Ours');
		expect(m.actionById.get('dup')?.source).toBe('project');
		expect(m.shadowedActions.map((a) => [a.id, a.overriddenBy])).toEqual([['dup', 'project']]);
	});

	it('file rules carry their layer and origin; rules without `when` read `always`', () => {
		const m = model({ files: makeFiles({ personalBindings: [{ key: 'mod+shift+e', command: 'pane.close' }] }) });
		const rule = m.keymap.entries.find((e) => e.source === 'personal');
		expect(rule).toMatchObject({ command: 'pane.close', when: 'always', origin: { scope: 'personal', index: 0 } });
	});

	it('drops a project OS rule (DEC-60: OS scope only from default + personal)', () => {
		const m = model({
			files: makeFiles({
				personalBindings: [{ key: 'ctrl+alt+space', command: 'os.summon', scope: 'os' }],
				projectBindings: [{ key: 'ctrl+alt+x', command: 'os.summon', scope: 'os' }],
			}),
		});
		// The default `os.*` rules (WP-54) are not file rules; only the
		// personal one of the two file rules survives.
		const os = m.keymap.entries.filter((e) => e.scope === 'os' && e.source !== 'default');
		expect(os.map((e) => [e.source, e.key])).toEqual([['personal', 'ctrl+alt+space']]);
	});
});

// ─── Negative rules ──────────────────────────────────────────────────────────

describe('negative rules (DEC-58, §1.5)', () => {
	it('removes that one binding and the key falls through', () => {
		const m = model({ files: makeFiles({ personalBindings: [{ key: 'mod+w', command: '-pane.close' }] }) });
		for (const platform of ['mac', 'other'] as const) {
			expect(onKey(m.keymap.entries, 'mod+w', platform)).toEqual([]);
		}
		expect(m.keymap.negatives).toEqual([
			{ scope: 'personal', index: 0, rule: { key: 'mod+w', command: '-pane.close' }, removed: 2 },
		]);
	});

	it('never tombstones the key: another command bound to it still resolves', () => {
		const m = model({
			files: makeFiles({
				personalActions: { version: 1, actions: [userAction('mine', 'personal')] },
				personalBindings: [
					{ key: 'mod+w', command: '-pane.close' },
					{ key: 'mod+w', command: 'mine', when: '!inputFocus' },
				],
			}),
		});
		expect(onKey(m.keymap.entries, 'mod+w', 'mac').map((e) => e.command)).toEqual(['mine']);
	});

	it('only removes earlier rules in merge order (a later positive survives)', () => {
		const m = model({
			files: makeFiles({
				personalBindings: [
					{ key: 'mod+w', command: '-pane.close' },
					{ key: 'mod+w', command: 'pane.close' },
				],
			}),
		});
		const hits = onKey(m.keymap.entries, 'mod+w', 'mac');
		expect(hits.map((e) => [e.command, e.source])).toEqual([['pane.close', 'personal']]);
	});

	it('with a `when` removes only the rule whose normalized `when` matches', () => {
		const keep = model({
			files: makeFiles({ personalBindings: [{ key: 'mod+w', command: '-pane.close', when: 'paneFocus' }] }),
		});
		expect(onKey(keep.keymap.entries, 'mod+w', 'mac').map((e) => e.command)).toEqual(['pane.close']);
		const drop = model({
			files: makeFiles({ personalBindings: [{ key: 'mod+w', command: '-pane.close', when: '!(inputFocus)' }] }),
		});
		expect(onKey(drop.keymap.entries, 'mod+w', 'mac')).toEqual([]);
	});

	it('acts only on its own platform: a mac-only negative leaves the Windows/Linux binding', () => {
		const m = model({
			files: makeFiles({ personalBindings: [{ key: 'mod+w', command: '-pane.close', platform: 'mac' }] }),
		});
		expect(onKey(m.keymap.entries, 'mod+w', 'mac')).toEqual([]);
		const other = onKey(m.keymap.entries, 'mod+w', 'other');
		expect(other.map((e) => [e.command, e.platformOnly])).toEqual([['pane.close', 'other']]);
	});

	it('matches on the platform-resolved key (`ctrl+w` only removes `mod+w` where mod is Ctrl)', () => {
		const m = model({ files: makeFiles({ personalBindings: [{ key: 'ctrl+w', command: '-pane.close' }] }) });
		expect(onKey(m.keymap.entries, 'mod+w', 'other')).toEqual([]);
		expect(onKey(m.keymap.entries, 'mod+w', 'mac').map((e) => e.command)).toEqual(['pane.close']);
	});

	it('a negative rule that matches nothing is inert and warned (W_NEGATIVE_NOOP)', () => {
		const m = model({ files: makeFiles({ personalBindings: [{ key: 'mod+shift+9', command: '-pane.close' }] }) });
		expect(m.keymap.negatives[0].removed).toBe(0);
		expect(m.issues).toContainEqual(
			expect.objectContaining({ code: 'W_NEGATIVE_NOOP', scope: 'personal', path: '/bindings/0' })
		);
		expect(onKey(m.keymap.entries, 'mod+w', 'mac').map((e) => e.command)).toEqual(['pane.close']);
	});

	it('a personal negative removes a granted package key', () => {
		const m = model({
			files: makeFiles({ personalBindings: [{ key: 'mod+shift+g', command: '-com.x.git:stage' }] }),
			packages: [pkgAction({ pkgId: 'com.x.git', localId: 'stage', keyRequest: 'mod+shift+g' })],
		});
		expect(m.keymap.packageRequests[0].byPlatform.mac).toEqual({ status: 'granted' });
		expect(m.keymap.entries.some((e) => e.command === 'com.x.git:stage')).toBe(false);
	});
});

// ─── Menus: locked-hide, hide ≠ unbind, overrides ────────────────────────────

describe('menus (§1.4, §9.2, DEC-58)', () => {
	it('rejects hiding a locked item in the merge itself (E_LOCKED_HIDDEN); the item stays', () => {
		const m = model({
			files: makeFiles({
				personalActions: { version: 1, menus: { files: { hidden: ['delete', 'copy-name'] } } },
			}),
		});
		const files = menuIds(m.menus.get('files'));
		expect(files).toContain('delete');
		expect(files).not.toContain('copy-name');
		expect(m.menus.get('files')?.hidden).toEqual(['copy-name']);
		expect(m.issues).toContainEqual(
			expect.objectContaining({
				level: 'error',
				code: 'E_LOCKED_HIDDEN',
				scope: 'personal',
				path: '/menus/files/hidden/0',
			})
		);
	});

	it('locked items can still be reordered', () => {
		const m = model({
			files: makeFiles({ personalActions: { version: 1, menus: { session: { items: ['kill-session', 'open'] } } } }),
		});
		expect(menuIds(m.menus.get('session')).slice(0, 2)).toEqual(['kill-session', 'open']);
	});

	it('a hidden built-in disappears from every menu and its key still resolves (hide ≠ unbind)', () => {
		const hideEverywhere = { hidden: ['pane.split-right'] };
		const m = model({
			files: makeFiles({
				personalActions: {
					version: 1,
					menus: { pane: hideEverywhere, palette: hideEverywhere, 'native/view': hideEverywhere },
				},
			}),
		});
		for (const menuId of m.menus.ids) {
			expect(menuIds(m.menus.get(menuId))).not.toContain('pane.split-right');
		}
		for (const platform of ['mac', 'other'] as const) {
			const candidates = onKey(m.keymap.entries, 'mod+\\', platform);
			expect(resolveKeypressWinner(candidates, m.keymap.entries)?.command).toBe('pane.split-right');
		}
	});

	it('items is an order: additions insert, omitted lower items append in lower order', () => {
		const m = model({
			files: makeFiles({
				personalActions: {
					version: 1,
					actions: [userAction('explain-file', 'personal')],
					menus: { todos: { items: ['hand-to-chi', '---', 'explain-file'] } },
				},
			}),
		});
		// `open-source` is not in `DEFAULT_MENUS.todos` any more (WP-56 A-9
		// cleanup, §10.3: todos carries no source-file reference, so it has no
		// real handler) — only `toggle-done` remains to append.
		expect(menuIds(m.menus.get('todos'))).toEqual(['hand-to-chi', '---', 'explain-file', 'toggle-done']);
	});

	it('hidden accumulates: personal hides, project reorders, the item stays hidden', () => {
		const m = model({
			files: makeFiles({
				personalActions: { version: 1, menus: { views: { hidden: ['pin-rail'] } } },
				projectActions: { version: 1, menus: { views: { items: ['pin-rail', 'open'] } } },
			}),
		});
		expect(menuIds(m.menus.get('views'))).toEqual(['open', 'open-to-side', 'open-in-ngwa']);
	});

	it('unknown ids in items / hidden are kept in the file, warned, never rendered', () => {
		const m = model({
			files: makeFiles({
				personalActions: { version: 1, menus: { views: { items: ['com.gone.pkg:x', 'open'], hidden: ['nope.missing'] } } },
			}),
		});
		expect(menuIds(m.menus.get('views'))).not.toContain('com.gone.pkg:x');
		const unknown = m.issues.filter((i) => i.code === 'W_UNKNOWN_COMMAND').map((i) => i.path);
		expect(unknown).toEqual(expect.arrayContaining(['/menus/views/items/0', '/menus/views/hidden/0']));
	});

	it('user placements join their layer; `section/<id>` menus resolve on demand', () => {
		const m = model({
			files: makeFiles({
				personalActions: {
					version: 1,
					actions: [userAction('refresh-pulse', 'personal', { placements: [{ at: 'section/automations' }] })],
				},
			}),
		});
		expect(m.menus.ids).toContain('section/automations');
		expect(menuIds(m.menus.get('section/automations'))).toEqual([
			'section.collapse-others',
			'section.hide',
			'---',
			'section.move-up',
			'section.move-down',
			'refresh-pulse',
		]);
		expect(menuIds(m.menus.get('section/files'))[0]).toBe('section.collapse-others');
		expect(m.menus.get('not-a-menu')).toBeNull();
	});

	it('collapses leading, trailing and doubled separators after hiding', () => {
		const m = model({
			files: makeFiles({ personalActions: { version: 1, menus: { todos: { items: ['---', 'toggle-done', '---', '---'] } } } }),
		});
		// `open-source` removed from `DEFAULT_MENUS.todos` (WP-56 A-9 cleanup) —
		// only `hand-to-chi` remains to append after the explicit items.
		expect(menuIds(m.menus.get('todos'))).toEqual(['toggle-done', '---', 'hand-to-chi']);
	});

	it('package `artifact` actions close the pane artifact branch, before the separator + pane.close (§7.3a)', () => {
		const m = model({ packages: [pkgAction({ pkgId: 'com.x', localId: 'export', selector: { kind: 'artifact' } })] });
		const ids = menuIds(m.menus.get('pane'));
		expect(ids.slice(-4)).toEqual(['viewer.toggle-history', 'com.x:export', '---', 'pane.close']);
		expect(menuIds(m.menus.get('artifacts')).at(-1)).toBe('com.x:export');
	});
});

// ─── Package actions and key requests ────────────────────────────────────────

describe('package key requests (DEC-54, §7.4, §12)', () => {
	it('grants a free key with the `when` derived from the selector (G-71)', () => {
		const m = model({
			packages: [
				pkgAction({
					pkgId: 'com.ikenga.git',
					localId: 'stage-file',
					selector: { kind: 'file', glob: '*.ts' },
					keyRequest: 'mod+shift+g',
				}),
			],
		});
		const granted = m.keymap.entries.filter((e) => e.command === 'com.ikenga.git:stage-file');
		expect(granted).toEqual([
			expect.objectContaining({
				key: 'mod+shift+g',
				when: "filesFocus && resource =~ '*.ts'",
				source: 'package',
				pkgId: 'com.ikenga.git',
			}),
		]);
		expect(granted[0].platformOnly).toBeUndefined();
	});

	it('lands unbound when the key is held by a default binding, whatever its `when`', () => {
		const m = model({ packages: [pkgAction({ pkgId: 'com.x', localId: 'a', keyRequest: 'mod+b' })] });
		expect(m.keymap.entries.some((e) => e.command === 'com.x:a')).toBe(false);
		expect(m.keymap.packageRequests[0].byPlatform.mac).toEqual({
			status: 'held',
			heldBy: { kind: 'binding', command: 'explorer.toggle', layer: 'default' },
		});
		// Still an action, still placed — only its key is missing.
		expect(m.actionById.get('com.x:a')?.source).toBe('package');
	});

	it('lands unbound when a later user binding takes the key (grants recomputed each merge)', () => {
		const pkg = [pkgAction({ pkgId: 'com.ikenga.git', localId: 'stage-file', keyRequest: 'mod+shift+g' })];
		expect(model({ packages: pkg }).keymap.entries.some((e) => e.source === 'package')).toBe(true);
		const m = model({
			packages: pkg,
			files: makeFiles({
				personalActions: { version: 1, actions: [userAction('mine', 'personal')] },
				personalBindings: [{ key: 'mod+shift+g', command: 'mine', when: 'paneFocus' }],
			}),
		});
		expect(m.keymap.entries.some((e) => e.source === 'package')).toBe(false);
		expect(m.keymap.packageRequests[0].byPlatform.other).toMatchObject({
			status: 'held',
			heldBy: { kind: 'binding', command: 'mine', layer: 'personal' },
		});
	});

	it('an earlier grant holds the key: the later package (grant order) arrives unbound', () => {
		const packages = readPackageActions({
			installed: [
				{ id: 'com.ikenga.git', installed_at: 10 },
				{ id: 'com.a.late', installed_at: 20 },
			],
			registries: {
				context_actions: {
					entries: [
						{
							pkg_id: 'com.a.late',
							qualified_id: 'com.a.late:commit-staged',
							id: 'commit-staged',
							label: 'Commit staged',
							when: { kind: 'file', glob: null },
							run: { kind: 'dispatch', prompt: 'commit', target: null },
							key: 'mod+shift+g',
						},
						{
							pkg_id: 'com.ikenga.git',
							qualified_id: 'com.ikenga.git:stage-file',
							id: 'stage-file',
							label: 'Stage file',
							when: { kind: 'file', glob: null },
							run: { kind: 'dispatch', prompt: 'stage', target: null },
							key: 'mod+shift+g',
						},
					],
				},
			},
		});
		expect(packages.map((p) => p.id)).toEqual(['com.ikenga.git:stage-file', 'com.a.late:commit-staged']);
		const m = model({ packages });
		expect(m.keymap.packageRequests.map((r) => r.byPlatform.mac.status)).toEqual(['granted', 'held']);
		expect(m.keymap.packageRequests[1].byPlatform.mac).toEqual({
			status: 'held',
			heldBy: { kind: 'package', command: 'com.ikenga.git:stage-file' },
		});
	});

	it('chord prefixes, OS keys and native-role accelerators hold keys too', () => {
		const m = model({
			files: makeFiles({
				personalActions: { version: 1, actions: [userAction('release-status', 'personal')] },
				personalBindings: [
					{ key: 'mod+k mod+r', command: 'release-status' },
					{ key: 'ctrl+alt+space', command: 'os.summon', scope: 'os' },
				],
			}),
			packages: [
				pkgAction({ pkgId: 'com.a', localId: 'r', keyRequest: 'mod+r' }),
				// ⌘Z: a native role, bound by no default (⌘C is now the hosted
				// `terminal.copy` on macOS, WP-54).
				pkgAction({ pkgId: 'com.b', localId: 'c', keyRequest: 'mod+z' }),
				pkgAction({ pkgId: 'com.c', localId: 's', keyRequest: 'ctrl+alt+space' }),
				pkgAction({ pkgId: 'com.d', localId: 'h', keyRequest: 'mod+h' }),
			],
		});
		const [r, c, s, h] = m.keymap.packageRequests;
		// The chord's SECOND stroke does not hold a single-stroke key.
		expect(r.byPlatform.mac.status).toBe('granted');
		expect(c.byPlatform.mac).toMatchObject({ status: 'held', heldBy: { kind: 'native-role' } });
		expect(s.byPlatform.mac).toMatchObject({ status: 'held', heldBy: { kind: 'os', command: 'os.summon' } });
		// ⌘H is a native role on macOS only.
		expect(h.byPlatform.mac).toMatchObject({ status: 'held', heldBy: { kind: 'native-role' } });
		expect(h.byPlatform.other.status).toBe('granted');
		expect(m.keymap.entries.find((e) => e.command === 'com.d:h')?.platformOnly).toBe('other');
	});

	it('a chord request is invalid (B-6), never granted', () => {
		const m = model({ packages: [pkgAction({ pkgId: 'com.a', localId: 'x', keyRequest: 'mod+k mod+x' })] });
		expect(m.keymap.packageRequests[0].byPlatform.mac.status).toBe('invalid');
		expect(m.keymap.entries.some((e) => e.command === 'com.a:x')).toBe(false);
	});

	it('command_palette[] entries are package actions placed in `palette`, shortcut `when` = !inputFocus (G-70)', () => {
		const packages = readPackageActions({
			installed: [{ id: 'com.x', installed_at: 1 }],
			registries: {
				context_actions: {
					count: 0,
					entries: [],
					command_palette: {
						count: 1,
						entries: [
							{
								pkg_id: 'com.x',
								qualified_id: 'com.x:sync',
								id: 'sync',
								label: 'Sync now',
								shortcut: 'mod+shift+y',
								action: { kind: 'view', route: '/pkg/com.x/sync' },
							},
						],
					},
				},
			},
		});
		const m = model({ packages });
		expect(m.actionById.get('com.x:sync')).toMatchObject({ source: 'package', packageOrigin: 'command_palette' });
		expect(menuIds(m.menus.get('palette'))).toContain('com.x:sync');
		expect(m.keymap.entries.find((e) => e.command === 'com.x:sync')).toMatchObject({
			key: 'mod+shift+y',
			when: '!inputFocus',
		});
	});

	it('an older shell snapshot without `command_palette` reads as no palette entries', () => {
		expect(readPackageActions({ registries: { context_actions: { count: 0, entries: [] } } })).toEqual([]);
		expect(readPackageActions(null)).toEqual([]);
	});
});

describe('selector derivations (§7.3, §7.3a)', () => {
	it('derives the key `when` per ContextSelector row (byte-for-byte with manifest.rs)', () => {
		expect(deriveContextActionKeyWhen({ kind: 'file' })).toBe('filesFocus');
		expect(deriveContextActionKeyWhen({ kind: 'file', glob: '' })).toBe('filesFocus');
		expect(deriveContextActionKeyWhen({ kind: 'file', glob: "it's/*.rs" })).toBe(
			"filesFocus && resource =~ 'it\\'s/*.rs'"
		);
		expect(deriveContextActionKeyWhen({ kind: 'artifact' })).toBe("paneKind == 'artifact'");
		expect(deriveContextActionKeyWhen({ kind: 'session' })).toBe('sessionFocus');
		expect(deriveContextActionKeyWhen({ kind: 'ngwa-item', kinds: [] })).toBe('ngwaItemFocus');
		expect(deriveContextActionKeyWhen({ kind: 'ngwa-item', kinds: ['task', 'run'] })).toBe(
			"ngwaItemFocus && (ngwaItemKind == 'task' || ngwaItemKind == 'run')"
		);
	});

	it('places selectors in their menus with the focus atom dropped', () => {
		const base = pkgAction({ pkgId: 'com.x', localId: 'a' });
		expect(packagePlacements({ ...base, selector: { kind: 'file', glob: '*.ts' } })).toEqual([
			{ at: 'files', when: "resource =~ '*.ts'", condition: 'file' },
		]);
		expect(packagePlacements({ ...base, selector: { kind: 'artifact' } })).toEqual([
			{ at: 'artifacts' },
			{ at: 'pane', condition: 'artifact-tab' },
		]);
		expect(packagePlacements({ ...base, selector: { kind: 'ngwa-item', kinds: ['task'] } })).toEqual([
			{ at: 'ngwa-project', when: "ngwaItemKind == 'task'" },
		]);
	});

	it('appends package context actions after the menu defaults', () => {
		const m = model({ packages: [pkgAction({ pkgId: 'com.x', localId: 'a', selector: { kind: 'session' } })] });
		const ids = menuIds(m.menus.get('session'));
		expect(ids[ids.length - 1]).toBe('com.x:a');
	});
});

// ─── DEC-65: untrusted project ───────────────────────────────────────────────

describe('untrusted project (DEC-65, §8.3)', () => {
	const projectBindings: KeybindingRule[] = [
		{ key: 'enter', command: 'delete', when: 'filesFocus' },
		{ key: 'mod+w', command: '-pane.close' },
		{ key: 'mod+shift+g', command: 'delete' },
	];
	const projectActions: ActionsDocument = { version: 1, menus: { files: { hidden: ['copy-name'] } } };

	for (const state of ['untrusted', 'changed'] as const) {
		it(`holds every project rule while ${state}: fires nothing, unbinds nothing; menu overrides apply`, () => {
			const m = model({
				files: makeFiles({ projectBindings, projectActions, projectTrust: state }),
				packages: [pkgAction({ pkgId: 'com.x', localId: 'g', keyRequest: 'mod+shift+g' })],
			});
			expect(isProjectKeybindingsHeld(m.files)).toBe(true);
			expect(m.keymap.projectHeld).toBe(true);
			expect(m.keymap.entries.some((e) => e.source === 'project')).toBe(false);
			expect(onKey(m.keymap.entries, 'enter', 'mac').map((e) => e.command)).not.toContain('delete');
			// The held negative unbinds nothing.
			expect(onKey(m.keymap.entries, 'mod+w', 'mac').map((e) => e.command)).toEqual(['pane.close']);
			// Held rules hold no key: the package request on the same key is granted.
			expect(m.keymap.packageRequests[0].byPlatform.mac.status).toBe('granted');
			// Not in conflicts().
			const all = [...m.keymap.conflicts.mac.clashes, ...m.keymap.conflicts.mac.precedence];
			expect(all.some((p) => p.a.source === 'project' || p.b.source === 'project')).toBe(false);
			// Exposed as held for the Keys tab.
			expect(m.keymap.held.map((h) => [h.index, h.trust])).toEqual([
				[0, state],
				[1, state],
				[2, state],
			]);
			// Menu overrides apply before trust.
			expect(menuIds(m.menus.get('files'))).not.toContain('copy-name');
		});
	}

	it('fails closed when trust cannot be read', () => {
		const files = makeFiles({ projectBindings });
		files.projectKeybindingsTrust = null;
		const m = model({ files });
		expect(m.keymap.projectHeld).toBe(true);
		expect(m.keymap.held[0].trust).toBe('unknown');
	});

	it('releases the rules once the keybindings are trusted', () => {
		const m = model({ files: makeFiles({ projectBindings, projectActions, projectTrust: 'trusted' }) });
		expect(m.keymap.held).toEqual([]);
		expect(onKey(m.keymap.entries, 'mod+w', 'mac')).toEqual([]);
		expect(onKey(m.keymap.entries, 'enter', 'mac').map((e) => e.command)).toContain('delete');
	});
});

// ─── Model warnings deferred by WP-50 ────────────────────────────────────────

describe('model warnings (§1.6)', () => {
	it('W_UNKNOWN_ICON checks the full Lucide list; the action renders zap', () => {
		const m = model({
			files: makeFiles({
				personalActions: {
					version: 1,
					actions: [
						userAction('a', 'personal', { icon: 'sparkles' }),
						userAction('b', 'personal', { icon: 'not-a-lucide-glyph' }),
					],
				},
			}),
		});
		expect(m.actionById.get('a')?.icon).toBe('sparkles');
		expect(m.actionById.get('b')?.icon).toBe('zap');
		expect(m.issues.filter((i) => i.code === 'W_UNKNOWN_ICON').map((i) => i.path)).toEqual(['/actions/1/icon']);
	});

	it('W_UNKNOWN_COMMAND for a binding to an id no source defines (rule kept, inert)', () => {
		const m = model({ files: makeFiles({ personalBindings: [{ key: 'mod+shift+j', command: 'com.gone:x' }] }) });
		expect(m.issues).toContainEqual(expect.objectContaining({ code: 'W_UNKNOWN_COMMAND', path: '/bindings/0' }));
	});
});
