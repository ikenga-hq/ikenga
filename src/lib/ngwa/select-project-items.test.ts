import { describe, expect, it } from 'vitest';
import { selectProjectNgwaItems } from './select-project-items';
import { mkItem } from '@/routes/ngwa/-ngwa-test-fixtures';

describe('selectProjectNgwaItems (DEC-74 / DEC-71)', () => {
	const personal = mkItem({ id: 'skill:personal:groundwork', kind: 'skill', name: 'groundwork' });
	const p1 = mkItem({
		id: 'skill:project:p1:other',
		kind: 'skill',
		name: 'other',
		scope: { kind: 'project', project_id: 'p1' },
	});
	const p2 = mkItem({
		id: 'agent:project:p2:explore',
		kind: 'agent',
		name: 'explore',
		scope: { kind: 'project', project_id: 'p2' },
	});
	const items = [personal, p1, p2];

	it('returns only items scoped to the given project id', () => {
		expect(selectProjectNgwaItems(items, 'p1')).toEqual([p1]);
		expect(selectProjectNgwaItems(items, 'p2')).toEqual([p2]);
	});

	it("maps 'default' to personal-scope items (DEC-71: Default project = personal)", () => {
		expect(selectProjectNgwaItems(items, 'default')).toEqual([personal]);
	});

	it('maps an empty project id to personal-scope items too', () => {
		expect(selectProjectNgwaItems(items, '')).toEqual([personal]);
	});

	it('returns no rows for a project with nothing scoped to it', () => {
		expect(selectProjectNgwaItems(items, 'p3')).toEqual([]);
	});

	it('returns [] for a null/undefined item list (still loading, or errored)', () => {
		expect(selectProjectNgwaItems(null, 'p1')).toEqual([]);
		expect(selectProjectNgwaItems(undefined, 'p1')).toEqual([]);
	});
});
