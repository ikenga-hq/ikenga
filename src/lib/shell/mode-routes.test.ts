// modeForRoute — route → owning activity mode (v16 / G-STATE).

import { describe, expect, it } from 'vitest';

import { modeForRoute } from './mode-routes';

describe('modeForRoute', () => {
	it('maps the exclusive v16 mode prefixes', () => {
		expect(modeForRoute('/project')).toBe('project');
		expect(modeForRoute('/project/dashboard')).toBe('project');
		expect(modeForRoute('/packages')).toBe('ngwa');
		expect(modeForRoute('/packages/browse')).toBe('ngwa');
		expect(modeForRoute('/packages?filter=review')).toBe('ngwa');
		expect(modeForRoute('/claude')).toBe('ngwa');
		expect(modeForRoute('/ngwa/skills')).toBe('ngwa');
		expect(modeForRoute('/settings/appearance')).toBe('settings');
		expect(modeForRoute('/chi')).toBe('chi');
		expect(modeForRoute('/chi/runs#latest')).toBe('chi');
	});

	it('returns null for package routes — they live under Project', () => {
		expect(modeForRoute('/pkg/com.ikenga.tasks/')).toBeNull();
		expect(modeForRoute('/pkg/com.ikenga.suite/sub/path')).toBeNull();
		expect(modeForRoute('/pkg/com.ikenga.tasks/?view=triage')).toBeNull();
	});

	it('returns null for routes shared across modes', () => {
		expect(modeForRoute('/')).toBeNull();
		expect(modeForRoute('/sessions')).toBeNull();
		expect(modeForRoute('/todos')).toBeNull();
		// Lookalike siblings must not match an exclusive prefix.
		expect(modeForRoute('/packages-foo')).toBeNull();
		expect(modeForRoute('/chimera')).toBeNull();
	});
});
