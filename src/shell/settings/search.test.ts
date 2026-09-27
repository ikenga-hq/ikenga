import { describe, expect, it } from 'vitest';

import { SETTINGS_SECTIONS } from './nav';
import { searchSettings } from './search';

// D-03 conformance: cross-section search must cover all nine sections
// (Secrets, Integrations and People included — search.tsx's own empty-state
// copy claims "in all nine sections"), and Secrets must never index a real
// secret value, only static row/control labels.
describe('settings cross-section search', () => {
	it('gives every one of the nine sections at least one searchable field', () => {
		expect(SETTINGS_SECTIONS).toHaveLength(9);
		for (const section of SETTINGS_SECTIONS) {
			expect(section.fields.length, `section "${section.id}" has no searchable fields`).toBeGreaterThan(0);
		}
	});

	it('finds a hit in Secrets without indexing any secret value', () => {
		const hits = searchSettings('passphrase');
		expect(hits.some((h) => h.sectionId === 'secrets')).toBe(true);
		for (const hit of hits) {
			expect(hit.label.toLowerCase()).not.toContain('sk-');
		}
	});

	it('finds hits in Integrations', () => {
		const hits = searchSettings('supabase');
		const sections = new Set(hits.map((h) => h.sectionId));
		expect(sections.has('integrations')).toBe(true);
	});

	it('finds hits in People & devices', () => {
		const hits = searchSettings('invite');
		expect(hits.some((h) => h.sectionId === 'people')).toBe(true);
	});

	it('returns nothing for a blank query', () => {
		expect(searchSettings('   ')).toEqual([]);
	});
});
