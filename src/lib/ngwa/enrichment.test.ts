import { describe, expect, it } from 'vitest';
import type { NgwaItem, NgwaTrust, NgwaUsage } from '@ikenga/contract';
import type { RegistryEntry } from '@/lib/registry/use-registry';
import {
	enrichNgwaItem,
	enrichNgwaItems,
	formatUsageDisplay,
	formatUsageTooltip,
	resolveTrustFacet,
	markTrustUnavailable,
	trustFacetLabel,
	buildStoreCatalog,
	mergeCatalogIntoStore,
	registryMatchesCatalog,
	storeKindFor,
} from './enrichment';
import type { PrimitiveCatalogEntry } from '@/lib/registry/primitives';
import type { ClaudeStoreEntry } from '@/lib/tauri-cmd';

function makeItem(partial: Partial<NgwaItem>): NgwaItem {
	const id = partial.id ?? 'test-item';
	const name = partial.name ?? id;
	return {
		id,
		kind: 'skill',
		name,
		display_name: partial.display_name ?? name,
		description: 'A test equipment item',
		version: '1.0.0',
		latest_version: null,
		scope: { kind: 'personal' },
		origin: {
			source: 'registry',
			url: null,
			ref: null,
			resolved_version: '1.0.0',
			publisher: null,
			managed: true,
			auto_update: false,
			installed_at_ms: 1000,
			updated_at_ms: 1000,
		},
		state: 'enabled',
		runtime: null,
		trust: {
			state: 'auto_trusted',
			signed: true,
			auto_trusted: true,
			review_pending: false,
			perms: null,
			last_granted_at_ms: null,
		},
		placements: [],
		usage: null,
		requires: [],
		required_by: [],
		owner_pkg_id: null,
		install_path: '/path/to/item',
		engines: ['claude'],
		...partial,
	};
}

describe('enrichment.ts', () => {
	describe('Gate §2 Usage null semantics', () => {
		it('renders null usage as em-dash "—", never as "0"', () => {
			expect(formatUsageDisplay(null)).toBe('—');
		});

		it('renders measured zero usage as "0 sessions", distinguishable from "—"', () => {
			const zeroUsage: NgwaUsage = {
				source: 'transcript',
				last_used_ms: null,
				count_7d: 0,
				count_30d: 0,
				tokens_30d: 0,
				window_start_ms: 1000,
			};
			const result = formatUsageDisplay(zeroUsage);
			expect(result).toBe('0 sessions');
			expect(result).not.toBe('—');
			expect(result).not.toBe('0');
		});

		it('renders positive session counts with "sessions"', () => {
			const activeUsage: NgwaUsage = {
				source: 'transcript',
				last_used_ms: 5000,
				count_7d: 12,
				count_30d: 45,
				tokens_30d: 150000,
				window_start_ms: 1000,
			};
			expect(formatUsageDisplay(activeUsage)).toBe('12 sessions');
		});

		it('honest tooltip explicitly notes cache-read tokens', () => {
			const activeUsage: NgwaUsage = {
				source: 'transcript',
				last_used_ms: 5000,
				count_7d: 12,
				count_30d: 45,
				tokens_30d: 4370000000,
				window_start_ms: 1000,
			};
			const tooltip = formatUsageTooltip(activeUsage);
			expect(tooltip).toContain('includes cache-read tokens');
			expect(tooltip).toContain('4,370,000,000 tokens in 30d');
		});

		it('tooltip for null usage states never measured', () => {
			expect(formatUsageTooltip(null)).toContain('Never measured');
		});
	});

	describe('Gate §13 latest_version and state: "update" enrichment', () => {
		const registryEntries: RegistryEntry[] = [
			{
				name: 'test-item',
				latest: '1.2.0',
				detail: 'https://example.com/test-1.2.0.json',
			},
			{
				name: '@ikenga/pkg-git',
				latest: '0.9.0',
				detail: 'https://example.com/git-0.9.0.json',
			},
		];

		it('fills latest_version and sets state: "update" when newer version exists', () => {
			const item = makeItem({ id: 'test-item', version: '1.0.0', state: 'enabled' });
			const enriched = enrichNgwaItem(item, registryEntries);
			expect(enriched.latest_version).toBe('1.2.0');
			expect(enriched.state).toBe('update');
		});

		it('keeps state: "enabled" when version matches latest', () => {
			const item = makeItem({ id: 'test-item', version: '1.2.0', state: 'enabled' });
			const enriched = enrichNgwaItem(item, registryEntries);
			expect(enriched.latest_version).toBe('1.2.0');
			expect(enriched.state).toBe('enabled');
		});

		it('matches reverse-DNS pkg id to @ikenga/pkg-<name>', () => {
			const item = makeItem({ id: 'com.ikenga.git', version: '0.8.0', state: 'enabled' });
			const enriched = enrichNgwaItem(item, registryEntries);
			expect(enriched.latest_version).toBe('0.9.0');
			expect(enriched.state).toBe('update');
		});

		it('leaves latest_version null if no registry match exists', () => {
			const item = makeItem({ id: 'local-only-tool', version: '0.1.0' });
			const enriched = enrichNgwaItem(item, registryEntries);
			expect(enriched.latest_version).toBeNull();
			expect(enriched.state).toBe('enabled');
		});

		it('enriches a list of items', () => {
			const items = [
				makeItem({ id: 'test-item', version: '1.0.0' }),
				makeItem({ id: 'local-tool', version: '0.5.0' }),
			];
			const enriched = enrichNgwaItems(items, registryEntries);
			expect(enriched[0].state).toBe('update');
			expect(enriched[0].latest_version).toBe('1.2.0');
			expect(enriched[1].state).toBe('enabled');
			expect(enriched[1].latest_version).toBeNull();
		});
	});

	describe('Gate §5 Trust facet derivation', () => {
		it('maps auto_trusted to "builtin"', () => {
			const trust: NgwaTrust = {
				state: 'auto_trusted',
				signed: false,
				auto_trusted: true,
				review_pending: false,
				perms: null,
				last_granted_at_ms: null,
			};
			expect(resolveTrustFacet(trust)).toBe('builtin');
		});

		it('maps signed manifest to "signed"', () => {
			const trust: NgwaTrust = {
				state: 'granted',
				signed: true,
				auto_trusted: false,
				review_pending: false,
				perms: null,
				last_granted_at_ms: null,
			};
			expect(resolveTrustFacet(trust)).toBe('signed');
		});

		it('maps unsigned non-review to "unsigned"', () => {
			const trust: NgwaTrust = {
				state: 'not_applicable',
				signed: false,
				auto_trusted: false,
				review_pending: false,
				perms: null,
				last_granted_at_ms: null,
			};
			expect(resolveTrustFacet(trust)).toBe('unsigned');
		});

		it('maps needs_approval or review_pending to "review"', () => {
			const t1: NgwaTrust = {
				state: 'needs_approval',
				signed: false,
				auto_trusted: false,
				review_pending: false,
				perms: null,
				last_granted_at_ms: null,
			};
			expect(resolveTrustFacet(t1)).toBe('review');

			const t2: NgwaTrust = {
				state: 'granted',
				signed: true,
				auto_trusted: false,
				review_pending: true,
				perms: null,
				last_granted_at_ms: null,
			};
			expect(resolveTrustFacet(t2)).toBe('review');
		});
	});

	describe('buildStoreCatalog', () => {
		const registryEntries: RegistryEntry[] = [
			{
				name: '@ikenga/pkg-tasks',
				latest: '0.8.3',
				detail: 'https://example.com/tasks.json',
			},
			{
				name: 'groundwork',
				latest: '0.7.6',
				detail: 'https://example.com/gw.json',
			},
			{
				name: 'new-uninstalled-skill',
				latest: '1.0.0',
				detail: 'https://example.com/new.json',
			},
		];

		it('identifies update available for installed item with older version', () => {
			const installed = [makeItem({ id: 'groundwork', name: 'groundwork', version: '0.7.4' })];
			const catalog = buildStoreCatalog(installed, registryEntries);
			const gw = catalog.find((c) => c.name === 'groundwork');
			expect(gw).toBeDefined();
			expect(gw?.isUpdate).toBe(true);
			expect(gw?.version).toBe('0.7.4');
			expect(gw?.latestVersion).toBe('0.7.6');
		});

		it('marks non-installed registry entries correctly', () => {
			const installed = [makeItem({ id: 'groundwork', name: 'groundwork', version: '0.7.6' })];
			const catalog = buildStoreCatalog(installed, registryEntries);
			const uninstalled = catalog.find((c) => c.name === 'new-uninstalled-skill');
			expect(uninstalled).toBeDefined();
			expect(uninstalled?.installedItem).toBeNull();
			expect(uninstalled?.isUpdate).toBe(false);
		});

		describe('visibility "hidden" (held-back apps)', () => {
			const hidden = (name: string, latest = '1.0.0'): RegistryEntry =>
				({ name, latest, detail: `pkgs/${name}.json`, visibility: 'hidden' }) as RegistryEntry;
			const publicEntry = (name: string, visibility?: 'public'): RegistryEntry =>
				({
					name,
					latest: '1.0.0',
					detail: `pkgs/${name}.json`,
					...(visibility ? { visibility } : {}),
				}) as RegistryEntry;

			it('keeps a hidden entry that is not installed out of browse and search', () => {
				const catalog = buildStoreCatalog(
					[],
					[publicEntry('@ikenga/pkg-tasks'), hidden('@ikenga/pkg-finance')]
				);
				expect(catalog.map((c) => c.name)).toEqual(['@ikenga/pkg-tasks']);
			});

			it('hides all seven held apps and leaves Tasks and Sales listed', () => {
				const held = [
					'@ikenga/pkg-finance',
					'@ikenga/pkg-mail',
					'@ikenga/pkg-content',
					'@ikenga/pkg-research',
					'@ikenga/pkg-strategy',
					'@ikenga/pkg-outbound',
					'@ikenga/pkg-agent-ops',
				];
				const catalog = buildStoreCatalog(
					[],
					[...held.map((n) => hidden(n)), publicEntry('@ikenga/pkg-tasks'), publicEntry('@ikenga/pkg-sales')]
				);
				expect(catalog.map((c) => c.name).sort()).toEqual([
					'@ikenga/pkg-sales',
					'@ikenga/pkg-tasks',
				]);
			});

			it('lists an entry marked "public" or with no flag', () => {
				const catalog = buildStoreCatalog(
					[],
					[publicEntry('@ikenga/pkg-a', 'public'), publicEntry('@ikenga/pkg-b')]
				);
				expect(catalog.map((c) => c.name)).toEqual(['@ikenga/pkg-a', '@ikenga/pkg-b']);
			});

			it('keeps a hidden entry that is installed, with update detection intact', () => {
				const installed = [
					makeItem({ id: '@ikenga/pkg-finance', name: '@ikenga/pkg-finance', version: '0.9.0' }),
				];
				const catalog = buildStoreCatalog(installed, [hidden('@ikenga/pkg-finance', '1.0.0')]);
				expect(catalog).toHaveLength(1);
				expect(catalog[0]?.installedItem).not.toBeNull();
				expect(catalog[0]?.isUpdate).toBe(true);
				expect(catalog[0]?.latestVersion).toBe('1.0.0');
			});

			it('does not change the registry list it was given, so exact-name lookups still resolve', () => {
				const entries = [hidden('@ikenga/pkg-finance'), publicEntry('@ikenga/pkg-tasks')];
				buildStoreCatalog([], entries);
				expect(entries.map((e) => e.name)).toEqual(['@ikenga/pkg-finance', '@ikenga/pkg-tasks']);
			});
		});
	});

	describe('storeKindFor — registry manifest hint → Ngwa kind', () => {
		const entry = (name: string, kind?: string): RegistryEntry => ({
			name,
			latest: '1.0.0',
			detail: `pkgs/${name}.json`,
			...(kind !== undefined ? { kind } : {}),
		});

		it('maps the manifest hints the index actually carries', () => {
			expect(storeKindFor(entry('@ikenga/pkg-agent-ops', 'embedded'), null)).toBe('app');
			expect(storeKindFor(entry('@ikenga/pkg-x', 'windowed'), null)).toBe('app');
			expect(storeKindFor(entry('@ikenga/pkg-meetings', 'app'), null)).toBe('app');
			expect(storeKindFor(entry('@ikenga/pkg-engine-codex', 'engine'), null)).toBe('engine');
			expect(storeKindFor(entry('@ikenga/studio-toolchain', 'bundle'), null)).toBe('bundle');
			expect(storeKindFor(entry('@ikenga/skill-groundwork', 'skill'), null)).toBe('skill');
		});

		it('classifies the @ikenga/mcp-* servers (hint "skill") as tools', () => {
			expect(storeKindFor(entry('@ikenga/mcp-browser', 'skill'), null)).toBe('tool');
			expect(storeKindFor(entry('@ikenga/mcp-iyke', 'skill'), null)).toBe('tool');
		});

		it('falls back to app for a missing or unknown hint', () => {
			expect(storeKindFor(entry('@ikenga/pkg-y'), null)).toBe('app');
			expect(storeKindFor(entry('@ikenga/pkg-z', 'mystery'), null)).toBe('app');
		});

		it('prefers the kernel kind of an installed pkg over the hint', () => {
			const installed = makeItem({ id: '@ikenga/pkg-git', kind: 'tool' });
			expect(storeKindFor(entry('@ikenga/pkg-git', 'embedded'), installed)).toBe('tool');
		});

		it('buildStoreCatalog uses the mapped kind', () => {
			const catalog = buildStoreCatalog(
				[],
				[entry('@ikenga/mcp-browser', 'skill'), entry('@ikenga/pkg-content', 'embedded')]
			);
			expect(catalog.map((c) => c.kind)).toEqual(['tool', 'app']);
		});
	});
});

// ── R57 · catalog rows + Q2 dedupe ────────────────────────────────────────────

describe('R57 · mergeCatalogIntoStore (Q2: one row per name)', () => {
	const SHA = '9c41e07a1b2c3d4e5f60718293a4b5c6d7e8f901';
	const cat = (name: string, over: Partial<PrimitiveCatalogEntry> = {}): PrimitiveCatalogEntry => ({
		kind: 'skill',
		name,
		version: '0.1.0',
		description: `${name} desc`,
		source: 'npx',
		url: `royalti-io/${name}`,
		publisher: 'royalti-io',
		...over,
	});
	const vaultEntry = (name: string, over: Partial<ClaudeStoreEntry> = {}): ClaudeStoreEntry => ({
		kind: 'skill',
		name,
		storePath: `/vault/skills/${name}`,
		description: null,
		modifiedMs: 0,
		enabledIn: ['workspace'],
		...over,
	});
	const registry = buildStoreCatalog(
		[],
		[
			{ name: '@ikenga/skill-groundwork', latest: '0.7.6', kind: 'skill' } as RegistryEntry,
			{ name: '@ikenga/pkg-tasks', latest: '0.8.3', kind: 'app' } as RegistryEntry,
		]
	);

	it('matches on kind + the registry name without scope and kind prefix', () => {
		expect(registryMatchesCatalog('@ikenga/skill-groundwork', 'skill', cat('groundwork'))).toBe(
			true
		);
		expect(registryMatchesCatalog('@ikenga/pkg-groundwork', 'skill', cat('groundwork'))).toBe(true);
		expect(registryMatchesCatalog('groundwork', 'skill', cat('groundwork'))).toBe(true);
		// Kinds never cross-match; other names never match.
		expect(registryMatchesCatalog('@ikenga/skill-groundwork', 'app', cat('groundwork'))).toBe(
			false
		);
		expect(
			registryMatchesCatalog(
				'@ikenga/skill-groundwork',
				'skill',
				cat('groundwork', { kind: 'agent' })
			)
		).toBe(false);
		expect(registryMatchesCatalog('@ikenga/skill-groundworks', 'skill', cat('groundwork'))).toBe(
			false
		);
	});

	it('folds the duplicate into the registry row with an `also` note', () => {
		const { registry: reg, primitives } = mergeCatalogIntoStore(
			registry,
			[cat('groundwork'), cat('impeccable')],
			[]
		);
		expect(primitives.map((p) => p.name)).toEqual(['impeccable']);
		const gw = reg.find((r) => r.name === '@ikenga/skill-groundwork');
		expect(gw?.alsoFrom).toEqual({ source: 'npx', url: 'royalti-io/groundwork' });
		expect(reg.find((r) => r.name === '@ikenga/pkg-tasks')?.alsoFrom).toBeUndefined();
		// The input rows are not mutated.
		expect(registry[0].alsoFrom).toBeUndefined();
	});

	it('builds catalog rows with installed state, mcp kind, and a pin-moved update', () => {
		const { primitives } = mergeCatalogIntoStore(
			[],
			[
				cat('design-language', { ref: SHA }),
				cat('subagent-model-floor', { kind: 'hook', source: 'git', url: 'https://x/hooks' }),
				cat('browser', { kind: 'mcp', source: 'git', url: 'https://x/mcp' }),
				cat('fresh'),
			],
			[
				vaultEntry('design-language', { version: '3e1a9c0ffff', fromCatalog: true }),
				vaultEntry('subagent-model-floor', { kind: 'hook', fromCatalog: true }),
			]
		);
		const by = Object.fromEntries(primitives.map((p) => [p.name, p]));
		expect(by['design-language'].id).toBe('cat:skill:design-language');
		expect(by['design-language'].installed?.name).toBe('design-language');
		expect(by['design-language'].isUpdate).toBe(true);
		expect(by['subagent-model-floor'].kind).toBe('hook');
		expect(by['subagent-model-floor'].isUpdate).toBe(false);
		expect(by.browser.kind).toBe('mcp');
		expect(by.browser.storeKind).toBe('mcp');
		expect(by.fresh.installed).toBeNull();
		expect(by.fresh.isUpdate).toBe(false);
	});

	it('a direct (non-catalog) install of a pinned name is not a catalog update', () => {
		const { primitives } = mergeCatalogIntoStore(
			[],
			[cat('design-language', { ref: SHA })],
			[vaultEntry('design-language', { version: '3e1a9c0ffff', fromCatalog: false })]
		);
		expect(primitives[0].isUpdate).toBe(false);
	});
});

describe('trust the server never evaluated (the headless daemon)', () => {
	const NA: NgwaTrust = {
		state: 'not_applicable',
		signed: false,
		auto_trusted: false,
		review_pending: false,
		perms: null,
		last_granted_at_ms: null,
	};
	const REASON = 'trust evaluation is not available on this server: no trust store';

	it('marks pkg items "unavailable" — never "unsigned" — and leaves primitives alone', () => {
		const pkg = makeItem({ id: 'com.x.app', name: 'com.x.app', kind: 'app', trust: NA });
		const skill = makeItem({ id: 'skill:personal:tidy', name: 'tidy', trust: NA });
		expect(resolveTrustFacet(pkg.trust)).toBe('unsigned');

		const [p, s] = markTrustUnavailable([pkg, skill], REASON);
		expect(resolveTrustFacet(p.trust)).toBe('unavailable');
		expect(trustFacetLabel(resolveTrustFacet(p.trust))).toBe('not available on this server');
		expect(s).toBe(skill);
	});

	it('is a no-op when trust was evaluated (the desktop)', () => {
		const items = [makeItem({ id: 'com.x.app', name: 'com.x.app', kind: 'app', trust: NA })];
		expect(markTrustUnavailable(items, null)).toBe(items);
	});

	it('never overrides an evaluated state', () => {
		const granted = makeItem({
			id: 'com.x.app',
			name: 'com.x.app',
			kind: 'app',
			trust: { ...NA, state: 'granted', signed: true },
		});
		expect(markTrustUnavailable([granted], REASON)[0]).toBe(granted);
	});
});
