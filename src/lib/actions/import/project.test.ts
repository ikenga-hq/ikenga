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

	it('always adds a negative rule (unbinding never overrides anything)', () => {
		const diff = buildProjectDiff(source({ bindings: [{ key: 'mod+w', command: '-pane.close' }] }), fakeModel());
		expect(diff.bindingRows[0].kind).toBe('add');
	});
});
