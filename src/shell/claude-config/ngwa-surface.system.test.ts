import { describe, expect, it } from 'vitest';

import type { ClaudeConfig, Project } from '@/lib/tauri-cmd';
import {
	buildItems,
	resolveActiveSystems,
	siblingSystemsOf,
	summarizeSystems,
	type NgwaItem,
	type NgwaSystemId,
} from './ngwa-helpers';

// WP-20 — the SYSTEM facet's pure logic: engine presence + per-engine counts,
// per-kind aggregation across active engines, the all-on default + stale-id
// guard in resolveActiveSystems, and the cross-engine collision detector.

const NO_PROJECTS: Project[] = [];

function agent(name: string, system: NgwaSystemId | undefined, format: 'md-yaml' | 'toml') {
	return {
		name,
		scope: 'personal' as const,
		projectRoot: null,
		path: `/home/dev/.${system ?? 'claude'}/agents/${name}.${format === 'toml' ? 'toml' : 'md'}`,
		modifiedMs: 0,
		description: null,
		model: null,
		frontmatter: {},
		body: '',
		overriddenBy: null,
		isSymlink: false,
		linkTarget: null,
		inStore: false,
		...(system ? { system } : {}),
		format,
		status: 'active' as const,
	};
}

function buildConfig(): ClaudeConfig {
	return {
		// rex: claude (via absent system ⇒ defaults to claude); reviewer under
		// both gemini + codex (cross-engine collision).
		agents: [
			agent('rex', undefined, 'md-yaml'),
			agent('reviewer', 'gemini', 'md-yaml'),
			agent('reviewer', 'codex', 'toml'),
		],
		skills: [],
		commands: [],
		hooks: [],
		mcps: [],
		errors: [],
	};
}

function items(): NgwaItem[] {
	return buildItems(buildConfig(), [], NO_PROJECTS);
}

describe('summarizeSystems', () => {
	it('treats absent system as claude and lists present engines in CL→GM→CX order', () => {
		const s = summarizeSystems(items());
		expect(s.present).toEqual(['claude', 'gemini', 'codex']);
		expect(s.engineCounts.claude).toBe(1);
		expect(s.engineCounts.gemini).toBe(1);
		expect(s.engineCounts.codex).toBe(1);
	});

	it('aggregates kind counts only across the active systems', () => {
		const s = summarizeSystems(items());
		// all three active → 3 agents
		expect(s.kindCounts(new Set(['claude', 'gemini', 'codex'])).agents).toBe(3);
		// only gemini active → just the gemini reviewer
		expect(s.kindCounts(new Set(['gemini'])).agents).toBe(1);
		// claude only → just rex
		expect(s.kindCounts(new Set(['claude'])).agents).toBe(1);
	});
});

describe('resolveActiveSystems', () => {
	const present: NgwaSystemId[] = ['claude', 'gemini', 'codex'];

	it('defaults to all present when selection is empty or null', () => {
		expect([...resolveActiveSystems(null, present)].sort()).toEqual(['claude', 'codex', 'gemini']);
		expect([...resolveActiveSystems([], present)].sort()).toEqual(['claude', 'codex', 'gemini']);
	});

	it('intersects the selection with present engines', () => {
		expect([...resolveActiveSystems(['gemini'], present)]).toEqual(['gemini']);
	});

	it('falls back to all present when the selection has no present engines', () => {
		// e.g. a stale `sys=cursor` after a re-scan dropped that engine.
		expect([...resolveActiveSystems(['cursor' as NgwaSystemId], present)].sort()).toEqual([
			'claude',
			'codex',
			'gemini',
		]);
	});

	it('drops absent engines from a mixed selection', () => {
		expect([...resolveActiveSystems(['gemini', 'cursor' as NgwaSystemId], present)]).toEqual([
			'gemini',
		]);
	});
});

describe('siblingSystemsOf', () => {
	it('finds the cross-engine collision for a same kind+name primitive', () => {
		const xs = items();
		const geminiReviewer = xs.find((i) => i.name === 'reviewer' && i.system === 'gemini')!;
		expect(siblingSystemsOf(geminiReviewer, xs)).toEqual(['codex']);
	});

	it('returns empty for a primitive with no same-named twin', () => {
		const xs = items();
		const rex = xs.find((i) => i.name === 'rex')!;
		expect(siblingSystemsOf(rex, xs)).toEqual([]);
	});

	it('returns empty for a null item', () => {
		expect(siblingSystemsOf(null, items())).toEqual([]);
	});
});
