// WP-61 unit tests (written under DEC-50, not run this session). Exercises
// `buildProjectDiff`'s action/binding classification. Like `vscode.test.ts`,
// relies on `keyHolder()` reading the DEFAULT-only keymap (no store harness).

import { describe, expect, it } from 'vitest';
import type { EffectiveModel, KeybindingRule, UserAction } from '@/lib/actions/store';
import { buildProjectDiff, type TeammateProjectSource } from './project';

function fakeModel(opts: { existingIds?: string[]; existingProjectBindings?: KeybindingRule[] } = {}): EffectiveModel {
	const ids = new Set(opts.existingIds ?? []);
	return {
		actionById: { has: (id: string) => ids.has(id) },
		files: opts.existingProjectBindings
			? { project: { keybindings: { document: { bindings: opts.existingProjectBindings } } } }
			: null,
	} as unknown as EffectiveModel;
}

function action(over: Partial<UserAction> = {}): UserAction {
	return {
		id: 'release-status',
		name: 'Release status',
		run: { kind: 'skill', skill: 'release-status' },
		scope: 'personal',
		...over,
	};
}

function source(over: Partial<TeammateProjectSource> = {}): TeammateProjectSource {
	return { actionsPath: '/teammate/.ikenga/actions.json', actions: [], bindings: [], bindingsPath: null, bindingsReadError: null, ...over };
}

describe('buildProjectDiff — actions', () => {
	it('adds a new action and forces its scope to project', () => {
		const diff = buildProjectDiff(source({ actions: [action()] }), fakeModel());
		expect(diff.actionRows[0].kind).toBe('add');
		expect(diff.actionRows[0].write?.scope).toBe('project');
	});

	it('skips an action whose id already exists', () => {
		const diff = buildProjectDiff(source({ actions: [action()] }), fakeModel({ existingIds: ['release-status'] }));
		expect(diff.actionRows[0].kind).toBe('skip');
	});

	it('notes the DEC-55 trust gate for a gated run kind', () => {
		const diff = buildProjectDiff(source({ actions: [action({ id: 'refresh-pulse', run: { kind: 'shell', command: 'x' } })] }), fakeModel());
		expect(diff.actionRows[0].detail).toMatch(/trusted/);
	});

	it('does not mention trust for an ungated (`chi`) run kind', () => {
		const diff = buildProjectDiff(
			source({ actions: [action({ id: 'explain-file', run: { kind: 'chi', target: 'active', prompt: 'x' } })] }),
			fakeModel()
		);
		expect(diff.actionRows[0].detail).not.toMatch(/trusted/);
	});
});

describe('buildProjectDiff — bindings', () => {
	it('adds a free binding, held until the project is trusted', () => {
		const diff = buildProjectDiff(source({ bindings: [{ key: 'mod+alt+shift+f17', command: 'release-status' }] }), fakeModel());
		expect(diff.bindingRows[0].kind).toBe('add');
		expect(diff.bindingRows[0].detail).toMatch(/held until/);
	});

	it('skips an exact duplicate of an existing project rule', () => {
		const rule: KeybindingRule = { key: 'mod+alt+shift+f17', command: 'release-status' };
		const diff = buildProjectDiff(source({ bindings: [rule] }), fakeModel({ existingProjectBindings: [rule] }));
		expect(diff.bindingRows[0].kind).toBe('skip');
	});

	it('reports a clash when the key is already held by a different command', () => {
		const diff = buildProjectDiff(source({ bindings: [{ key: 'mod+b', command: 'release-status' }] }), fakeModel());
		expect(diff.bindingRows[0].kind).toBe('clash');
	});

	it('skips a negative rule — a teammate import never removes a binding (fix round 1, item 3)', () => {
		const diff = buildProjectDiff(source({ bindings: [{ key: 'mod+w', command: '-pane.close' }] }), fakeModel());
		expect(diff.bindingRows[0].kind).toBe('skip');
		expect(diff.bindingRows[0].detail).toMatch(/removes a binding/);
	});

	it('skips a project `scope: "os"` rule (DEC-60 — only the personal file may be OS-wide)', () => {
		const diff = buildProjectDiff(
			source({ bindings: [{ key: 'alt+space', command: 'os.summon', scope: 'os' }] }),
			fakeModel()
		);
		expect(diff.bindingRows[0].kind).toBe('skip');
		expect(diff.bindingRows[0].detail).toMatch(/personal file/);
	});

	it('skips an invalid key with a reason, before ever checking who holds it', () => {
		const diff = buildProjectDiff(source({ bindings: [{ key: 'ctrl+meta+shift+unknownkey', command: 'release-status' }] }), fakeModel());
		expect(diff.bindingRows[0].kind).toBe('skip');
		expect(diff.bindingRows[0].detail).toMatch(/invalid key/);
	});

	it('strips a stray `action:` prefix from the command (§10.1)', () => {
		const diff = buildProjectDiff(source({ bindings: [{ key: 'mod+alt+shift+f18', command: 'action:release-status' }] }), fakeModel());
		expect(diff.bindingRows[0].kind).toBe('add');
		expect(diff.bindingRows[0].write?.command).toBe('release-status');
	});

	it('skips the second of two rows in the same import asking for the same free key', () => {
		const diff = buildProjectDiff(
			source({
				bindings: [
					{ key: 'mod+alt+shift+f17', command: 'release-status' },
					{ key: 'mod+alt+shift+f17', command: 'explain-file' },
				],
			}),
			fakeModel()
		);
		expect(diff.bindingRows[0].kind).toBe('add');
		expect(diff.bindingRows[1].kind).toBe('clash');
		expect(diff.bindingRows[1].detail).toMatch(/earlier in this import/);
	});

	it('reports "already bound", not a clash, when the key is held by the same command (fix round 1, item 7)', () => {
		// `keyHolder` reads the DEFAULT-only keymap (see the file header note),
		// so this uses a default-layer command — `mod+b` is `explorer.toggle`'s
		// own default key (G-ACTIONS §10.2) — as the "same id" under test.
		const diff = buildProjectDiff(source({ bindings: [{ key: 'mod+b', command: 'explorer.toggle' }] }), fakeModel());
		expect(diff.bindingRows[0].kind).toBe('skip');
		expect(diff.bindingRows[0].detail).toBe('already bound');
	});
});
