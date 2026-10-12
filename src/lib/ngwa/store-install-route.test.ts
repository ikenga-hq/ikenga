// Where a registry row installs from, per host (browser session vs desktop).

import { describe, expect, it } from 'vitest';
import type { NgwaStoreEntry } from '@/lib/ngwa/enrichment';
import type { PrimitiveCatalogEntry } from '@/lib/registry/primitives';
import type { ClaudeStoreEntry } from '@/lib/tauri-cmd';
import { altCatalogRow, nonSkillBlocks, storeInstallRoute } from './store-install-route';
import type { StoreManifest } from './store-detail';

const SHA = '9c41e07a1b2c3d4e5f60718293a4b5c6d7e8f901';
const HASH = `sha256-${'ab'.repeat(32)}`;
const REASON = 'Packages are installed by the server operator';

const alt = (over: Partial<PrimitiveCatalogEntry> = {}): PrimitiveCatalogEntry => ({
	kind: 'skill',
	name: 'groundwork',
	version: '0.3.0',
	description: 'Plan folders',
	source: 'npx',
	url: 'royalti-io/groundwork',
	ref: SHA,
	hash: HASH,
	...over,
});

function entry(over: Partial<NgwaStoreEntry> = {}): NgwaStoreEntry {
	return {
		id: '@ikenga/skill-groundwork',
		name: '@ikenga/skill-groundwork',
		displayName: 'skill-groundwork',
		description: 'Groundwork',
		version: '1.0.0',
		latestVersion: '1.0.0',
		kind: 'skill',
		trustFacet: 'signed',
		installedItem: null,
		isUpdate: false,
		registryEntry: { name: '@ikenga/skill-groundwork', latest: '1.0.0' },
		alsoFrom: { source: 'npx', url: 'royalti-io/groundwork' },
		alsoEntry: alt(),
		...over,
	} as NgwaStoreEntry;
}

const manifest = (over: Record<string, unknown> = {}) =>
	({
		id: 'x',
		name: 'x',
		version: '1.0.0',
		ikenga_api: '1',
		mcp: [],
		sidecars: [],
		requires: [],
		permissions: {},
		ui: {},
		...over,
	}) as unknown as StoreManifest;

const BROWSER = { pkg: false, oba: true, reason: REASON };
const DESKTOP = { pkg: true, oba: true };

describe('storeInstallRoute', () => {
	it('desktop: always the pkg kernel, even for a skill with a twin', () => {
		expect(storeInstallRoute(entry(), DESKTOP, null, [])).toEqual({ via: 'pkg' });
	});

	it('browser: a skill with an npx twin installs through Ọba, naming the source', () => {
		const r = storeInstallRoute(entry(), BROWSER, null, []);
		expect(r.via).toBe('oba');
		if (r.via !== 'oba') return;
		expect(r.note).toBe('Installs the skill into your account (npx source)');
		expect(r.pinned).toBe(true);
		expect(r.row.entry.url).toBe('royalti-io/groundwork');
		expect(r.row.id).toBe('cat:skill:groundwork');
	});

	it.each([
		'hook',
		'agent',
		'command',
		'mcp',
	] as const)('browser: a registry %s with a same-kind catalog twin never takes the Ọba route', (kind) => {
		// Only skills and bundles may fall back to their catalog twin: a hook or MCP
		// twin would install without the "runs" consent the catalog sheet asks for.
		const e = entry({
			kind,
			alsoEntry: alt({ kind, name: 'groundwork' }),
		} as Partial<NgwaStoreEntry>);
		expect(altCatalogRow(e, [])).toBeNull();
		expect(storeInstallRoute(e, BROWSER, null, []).via).toBe('blocked');
	});

	it('browser: a bundle with a twin installs through Ọba too', () => {
		const r = storeInstallRoute(
			entry({ kind: 'bundle', alsoEntry: alt({ kind: 'bundle', name: 'studio-archetypes' }) }),
			BROWSER,
			null,
			[]
		);
		expect(r.via === 'oba' && r.note).toBe('Installs the bundle into your account (npx source)');
	});

	it('browser: an unpinned twin is routed but flagged unpinned', () => {
		const r = storeInstallRoute(
			entry({ alsoEntry: alt({ ref: undefined, hash: undefined }) }),
			BROWSER,
			null,
			[]
		);
		expect(r.via === 'oba' && r.pinned).toBe(false);
	});

	it.each([
		['an app', { kind: 'app' as const }],
		['an engine', { kind: 'engine' as const }],
		['a tool', { kind: 'tool' as const }],
		['a skill with no twin', { alsoFrom: null, alsoEntry: null }],
		['a pkg that failed to load', { broken: 'register_failed' }],
	])('browser: %s is blocked with the operator reason', (_n, over) => {
		expect(storeInstallRoute(entry(over), BROWSER, null, [])).toEqual({
			via: 'blocked',
			reason: REASON,
		});
	});

	it('browser: no Ọba handler → blocked', () => {
		expect(storeInstallRoute(entry(), { ...BROWSER, oba: false }, null, [])).toEqual({
			via: 'blocked',
			reason: REASON,
		});
	});

	it('browser: reads the manifest — a skill that ships a server / sidecar / UI / permission is blocked', () => {
		expect(storeInstallRoute(entry(), BROWSER, manifest(), []).via).toBe('oba');
		for (const over of [
			{ mcp: [{ name: 's' }] },
			{ sidecars: [{ name: 's' }] },
			{ ui: { routes: [{ path: '/' }] } },
			{ engine: { id: 'e' } },
			{ permissions: { net: ['https://a.example'] } },
		]) {
			expect(storeInstallRoute(entry(), BROWSER, manifest(over), []).via).toBe('blocked');
		}
	});

	it('browser: falls back to "not available" without an operator reason', () => {
		expect(
			storeInstallRoute(entry({ alsoEntry: null }), { pkg: false, oba: true }, null, [])
		).toEqual({
			via: 'blocked',
			reason: 'Not available on this server yet',
		});
	});

	it('carries the vault record so an installed twin reads installed', () => {
		const v: ClaudeStoreEntry = {
			kind: 'skill',
			name: 'groundwork',
			storePath: '/v/groundwork',
			description: null,
			modifiedMs: 0,
			enabledIn: ['workspace'],
		};
		const r = storeInstallRoute(entry(), BROWSER, null, [v]);
		expect(r.via === 'oba' && r.row.installed).toEqual(v);
	});
});

describe('altCatalogRow / nonSkillBlocks', () => {
	it('refuses a kind mismatch between the registry row and its twin', () => {
		expect(altCatalogRow(entry({ alsoEntry: alt({ kind: 'agent' }) }), [])).toBeNull();
	});
	it('names which blocks make a pkg more than a skill', () => {
		expect(nonSkillBlocks(manifest())).toEqual([]);
		expect(nonSkillBlocks(manifest({ mcp: [{}], cron: [{}] }))).toEqual(['mcp', 'cron']);
	});
});
