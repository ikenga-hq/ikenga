import { describe, expect, it } from 'vitest';
import type { NgwaItem, NgwaTrust, NgwaUsage } from '@ikenga/contract';
import type { RegistryEntry } from '@/lib/registry/use-registry';
import {
	enrichNgwaItem,
	enrichNgwaItems,
	formatUsageDisplay,
	formatUsageTooltip,
	resolveTrustFacet,
	buildStoreCatalog,
} from './enrichment';

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
			const installed = [
				makeItem({ id: 'groundwork', name: 'groundwork', version: '0.7.4' }),
			];
			const catalog = buildStoreCatalog(installed, registryEntries);
			const gw = catalog.find((c) => c.name === 'groundwork');
			expect(gw).toBeDefined();
			expect(gw?.isUpdate).toBe(true);
			expect(gw?.version).toBe('0.7.4');
			expect(gw?.latestVersion).toBe('0.7.6');
		});

		it('marks non-installed registry entries correctly', () => {
			const installed = [
				makeItem({ id: 'groundwork', name: 'groundwork', version: '0.7.6' }),
			];
			const catalog = buildStoreCatalog(installed, registryEntries);
			const uninstalled = catalog.find((c) => c.name === 'new-uninstalled-skill');
			expect(uninstalled).toBeDefined();
			expect(uninstalled?.installedItem).toBeNull();
			expect(uninstalled?.isUpdate).toBe(false);
		});
	});
});
