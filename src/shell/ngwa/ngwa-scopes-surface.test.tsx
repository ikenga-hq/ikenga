// Ngwa Scopes model tests (WP-16a). Pure functions only; the interactive
// behaviour is covered route-level in src/routes/ngwa/-scopes-route.test.tsx.

import { describe, expect, it } from 'vitest';
import {
	buildRows,
	conflictOf,
	engineIdOfPkg,
	engineMark,
	installedEngines,
	isPkgItem,
	scopeMark,
	wireOf,
} from './ngwa-scopes-surface';
import {
	engineItems,
	mkItem,
	mkPlacement,
	scopesItems,
} from '@/routes/ngwa/-ngwa-test-fixtures';

describe('scopes model', () => {
	it('maps engine pkgs to engine ids; other engines draw no column', () => {
		expect(engineIdOfPkg(engineItems(['claude'])[0])).toBe('claude');
		expect(engineIdOfPkg(engineItems(['codex'])[0])).toBe('codex');
		expect(engineIdOfPkg(engineItems(['gemini'])[0])).toBe('gemini');
		expect(
			engineIdOfPkg(mkItem({ id: 'com.ikenga.engine-opencode', kind: 'engine', name: 'com.ikenga.engine-opencode' }))
		).toBeNull();
	});

	it('installedEngines reads engine pkgs only, never placements', () => {
		const placedOnly = mkItem({
			id: 'skill:personal:x',
			kind: 'skill',
			name: 'x',
			engines: ['gemini'],
			placements: [mkPlacement({ engine: 'gemini', path: '/g/x' })],
		});
		expect([...installedEngines([placedOnly]).keys()]).toEqual([]);
		expect([...installedEngines(engineItems(['claude', 'codex'])).keys()]).toEqual(['claude', 'codex']);
	});

	it('distinguishes pkgs from config-scan items of the same kind', () => {
		expect(isPkgItem(mkItem({ id: 'com.x.tool', kind: 'tool', name: 'com.x.tool' }))).toBe(true);
		expect(isPkgItem(mkItem({ id: 'tool:personal:fs', kind: 'tool', name: 'fs' }))).toBe(false);
	});

	it('maps personal to workspace and projects to project:<id>', () => {
		expect(wireOf('personal')).toBe('workspace');
		expect(wireOf('project:p9')).toBe('project:p9');
	});

	it('conflict comes from overridden_by and resolves the shadowing project item', () => {
		const rows = buildRows(scopesItems());
		const gw = rows.find((r) => r.key === 'prim:skill:groundwork');
		const c = gw ? conflictOf(gw) : null;
		expect(c?.project?.id).toBe('skill:project:p1:groundwork');
		expect(gw && scopeMark(gw, 'personal', c)).toBe('conflict');
		const agent = rows.find((r) => r.key === 'prim:agent:groundwork');
		expect(agent && conflictOf(agent)).toBeNull();
	});

	it('engine marks union placements across scope items', () => {
		const lint = buildRows(scopesItems()).find((r) => r.key === 'prim:skill:lint');
		expect(lint && engineMark(lint, 'codex')).toBe('link');
		expect(lint && engineMark(lint, 'gemini')).toBe('none');
	});
});
