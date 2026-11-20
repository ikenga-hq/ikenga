import { describe, expect, it } from 'vitest';
import { encodeUrlPath } from './path';

describe('encodeUrlPath', () => {
	it('escapes characters that break a URL path', () => {
		expect(encodeUrlPath('Report #2 50%.html')).toBe('Report%20%232%2050%25.html');
		expect(encodeUrlPath('a b#c.mp3')).toBe('a%20b%23c.mp3');
		expect(encodeUrlPath('what?.mp4')).toBe('what%3F.mp4');
	});

	it('keeps / separators between segments', () => {
		expect(encodeUrlPath('sub dir/page #1.html')).toBe('sub%20dir/page%20%231.html');
	});

	it('leaves plain names alone', () => {
		expect(encodeUrlPath('index.html')).toBe('index.html');
	});
});
