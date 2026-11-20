import { describe, expect, it } from 'vitest';
import { DEFAULT_MENUS } from '@/lib/actions/menus';
import { canShareFile, projectRelativePath } from './share-file';

describe('projectRelativePath', () => {
	it('returns the path inside the project folder', () => {
		expect(projectRelativePath('/srv/p/docs/a.pdf', '/srv/p')).toBe('docs/a.pdf');
		expect(projectRelativePath('/srv/p/docs/a.pdf', '/srv/p/')).toBe('docs/a.pdf');
		expect(projectRelativePath('C:\\p\\a.md', 'C:\\p')).toBe('a.md');
	});

	it('refuses a path outside, equal to, or escaping the folder', () => {
		expect(projectRelativePath('/srv/other/a.pdf', '/srv/p')).toBeNull();
		expect(projectRelativePath('/srv/p', '/srv/p')).toBeNull();
		expect(projectRelativePath('/srv/pp/a.pdf', '/srv/p')).toBeNull();
		expect(projectRelativePath('/srv/p/../x/a.pdf', '/srv/p')).toBeNull();
		expect(projectRelativePath('/srv/p/a.pdf', null)).toBeNull();
	});
});

describe('canShareFile', () => {
	const ok = { tier: 't1' as const, adminStrength: true, inShare: false, root: '/srv/p', path: '/srv/p/a.pdf' };

	it('is on for an owner-strength T1 session and a file in the project', () => {
		expect(canShareFile(ok)).toBe(true);
	});

	it('is off on the desktop / T0, inside a share, without admin strength, or outside the project', () => {
		expect(canShareFile({ ...ok, tier: 't0' })).toBe(false);
		expect(canShareFile({ ...ok, inShare: true })).toBe(false);
		expect(canShareFile({ ...ok, adminStrength: false })).toBe(false);
		expect(canShareFile({ ...ok, path: '/srv/other/a.pdf' })).toBe(false);
		expect(canShareFile({ ...ok, root: null })).toBe(false);
	});
});

describe('files.share placement', () => {
	it('sits after Download in the Files row menu and the viewer pane menu', () => {
		for (const menu of ['files', 'pane'] as const) {
			const ids = DEFAULT_MENUS[menu].map((e) => (typeof e === 'string' ? e : e.id));
			expect(ids.indexOf('files.share')).toBe(ids.indexOf('files.download') + 1);
			const entry = DEFAULT_MENUS[menu].find((e) => typeof e !== 'string' && e.id === 'files.share');
			expect(entry).toMatchObject({ condition: 'shareable' });
		}
	});
});
