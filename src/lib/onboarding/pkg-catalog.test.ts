import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

import { resolveRequiredConnectors } from './resolve-connectors';
import {
	countByBucket,
	defaultSelectedIds,
	findCatalogEntry,
	ONBOARDING_PKG_CATALOG,
} from './pkg-catalog';

// Apps the public registry holds back (`visibility: "hidden"`) until they work
// on a fresh install. The first-run wizard must neither list nor pre-select them.
const HELD_APP_IDS = [
	'com.ikenga.finance',
	'com.ikenga.mail',
	'com.ikenga.content',
	'com.ikenga.research',
	'com.ikenga.strategy',
	'com.ikenga.outbound',
	'com.ikenga.agent-ops',
] as const;

describe('onboarding pkg catalog', () => {
	const ids = ONBOARDING_PKG_CATALOG.map((p) => p.manifest.id);

	it('does not offer any held app', () => {
		for (const id of HELD_APP_IDS) {
			expect(ids, `${id} must not be offered`).not.toContain(id);
			expect(findCatalogEntry(id)).toBeUndefined();
		}
	});

	it('does not pre-select any held app', () => {
		const selected = defaultSelectedIds();
		for (const id of HELD_APP_IDS) expect(selected).not.toContain(id);
	});

	it('does not offer the Files entry, which the registry does not carry', () => {
		expect(ids).not.toContain('com.ikenga.files');
	});

	it('pre-selects Tasks but not Sales, and lists both', () => {
		const selected = defaultSelectedIds();
		expect(selected).toContain('com.ikenga.tasks');
		expect(selected).not.toContain('com.ikenga.sales');
		expect(findCatalogEntry('com.ikenga.sales')?.defaultSelected).toBe(false);
	});

	it('describes Tasks and Sales as local-only, with no cloud connector required', () => {
		for (const id of ['com.ikenga.tasks', 'com.ikenga.sales']) {
			const entry = findCatalogEntry(id);
			expect(entry?.bucket, id).toBe('local-only');
			expect(entry?.manifest.capabilities?.supabase, id).toBeUndefined();
			expect(entry?.manifest.permissions?.['vault.keys'], id).toEqual([]);
		}
		const requirements = resolveRequiredConnectors(
			['com.ikenga.tasks', 'com.ikenga.sales'],
			ONBOARDING_PKG_CATALOG.map((p) => p.manifest)
		);
		expect(requirements).toEqual([]);
	});

	it('carries the registry version for Tasks and Sales', () => {
		expect(findCatalogEntry('com.ikenga.tasks')?.version).toBe('0.8.4');
		expect(findCatalogEntry('com.ikenga.tasks')?.manifest.version).toBe('0.8.4');
		expect(findCatalogEntry('com.ikenga.sales')?.version).toBe('0.4.1');
		expect(findCatalogEntry('com.ikenga.sales')?.manifest.version).toBe('0.4.1');
	});

	it('no longer has any needs-cloud entry', () => {
		expect(countByBucket()['needs-cloud']).toBe(0);
	});

	it('keeps the manifest id and the entry version in step', () => {
		for (const entry of ONBOARDING_PKG_CATALOG) {
			expect(entry.manifest.version, entry.manifest.id).toBe(entry.version);
		}
	});
});

describe('onboarding catalog and the install catalog', () => {
	// The install queue resolves a selected id to a local path through
	// `public/install-catalog.json`. Neither list may name a held app, so the two
	// cannot disagree about what a first run offers.
	const here = dirname(fileURLToPath(import.meta.url));
	const installCatalog = JSON.parse(
		readFileSync(join(here, '..', '..', '..', 'public', 'install-catalog.json'), 'utf8')
	) as { packages?: Array<{ id: string }> };
	const installIds = (installCatalog.packages ?? []).map((p) => p.id);

	it('the install catalog does not resolve any held app', () => {
		for (const id of HELD_APP_IDS) expect(installIds).not.toContain(id);
	});

	it('every onboarding entry that the install catalog resolves is one the wizard lists', () => {
		const listed = new Set(ONBOARDING_PKG_CATALOG.map((p) => p.manifest.id));
		const resolvable = installIds.filter((id) => id.startsWith('com.ikenga.'));
		for (const id of resolvable) expect(listed.has(id), `${id} resolves but is not offered`).toBe(true);
	});
});
