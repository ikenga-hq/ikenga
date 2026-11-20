import { describe, expect, it } from 'vitest';
import { classifyImageSrc, hasImageRefs, makeImageResolver } from './markdown-assets';

describe('classifyImageSrc', () => {
	it('leaves absolute and data URLs alone', () => {
		expect(classifyImageSrc('https://x.test/a.png')).toEqual({
			kind: 'external',
			src: 'https://x.test/a.png',
		});
		expect(classifyImageSrc('http://x.test/a.png').kind).toBe('external');
		expect(classifyImageSrc('data:image/png;base64,AAAA').kind).toBe('external');
		expect(classifyImageSrc('//cdn.test/a.png').kind).toBe('external');
	});
	it('resolves relative paths inside the folder, encoding segments', () => {
		expect(classifyImageSrc('./chart.png')).toEqual({ kind: 'relative', rel: 'chart.png' });
		expect(classifyImageSrc('img/a b.png?x=1#y')).toEqual({
			kind: 'relative',
			rel: 'img/a%20b.png',
		});
		expect(classifyImageSrc('a/../b.png')).toEqual({ kind: 'relative', rel: 'b.png' });
		expect(classifyImageSrc('a%23b.png')).toEqual({ kind: 'relative', rel: 'a%23b.png' });
	});
	it('refuses anything that climbs out or is rooted', () => {
		expect(classifyImageSrc('../secret.png').kind).toBe('blocked');
		expect(classifyImageSrc('a/../../x.png').kind).toBe('blocked');
		expect(classifyImageSrc('%2e%2e/x.png').kind).toBe('blocked');
		expect(classifyImageSrc('..\\x.png').kind).toBe('blocked');
		expect(classifyImageSrc('/etc/passwd').kind).toBe('blocked');
		expect(classifyImageSrc('~/x.png').kind).toBe('blocked');
		expect(classifyImageSrc('').kind).toBe('blocked');
	});
});

describe('makeImageResolver', () => {
	it('joins relative sources onto the mount base', () => {
		const r = makeImageResolver('http://localhost:1/v/t/');
		expect(r('./chart.png')).toBe('http://localhost:1/v/t/chart.png');
		expect(r('https://x.test/a.png')).toBe('https://x.test/a.png');
		expect(r('../x.png')).toBeNull();
	});
	it('is pending (undefined) for relative sources until the mount is ready', () => {
		const r = makeImageResolver(null);
		expect(r('chart.png')).toBeUndefined();
		expect(r('https://x.test/a.png')).toBe('https://x.test/a.png');
	});
});

describe('hasImageRefs', () => {
	it('detects markdown and html images only', () => {
		expect(hasImageRefs('see ![alt](./a.png)')).toBe(true);
		expect(hasImageRefs('<img src="a.png">')).toBe(true);
		expect(hasImageRefs('a [link](x.md) only')).toBe(false);
	});
});
