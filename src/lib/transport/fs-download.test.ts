import { describe, expect, it } from 'vitest';
import { fsDownloadUrl } from './fs-download';

describe('fsDownloadUrl', () => {
	it('encodes the path as one query value', () => {
		const u = new URL(fsDownloadUrl('/home/a b/r\u00e9sum\u00e9 & #1?.pdf'), 'http://x');
		expect(u.pathname).toBe('/api/fs/download');
		expect(u.searchParams.get('path')).toBe('/home/a b/r\u00e9sum\u00e9 & #1?.pdf');
		expect(u.searchParams.has('inline')).toBe(false);
	});

	it('adds inline=1 only when asked', () => {
		expect(fsDownloadUrl('/a.pdf', { inline: true })).toContain('inline=1');
		expect(fsDownloadUrl('/a.pdf', { inline: false })).not.toContain('inline');
	});
});
