import { describe, expect, it } from 'vitest';
import {
	hasBalancedParens,
	looksLikePath,
	normalizePath,
	resolvePath,
	resolvePathCandidates,
} from './file-paths';

describe('file-paths', () => {
	it('resolves paths synchronously with resolvePath', () => {
		expect(resolvePath('src/file.ts', '/workspace')).toBe('/workspace/src/file.ts');
		expect(resolvePath('/abs/file.ts', '/workspace')).toBe('/abs/file.ts');
	});

	it('checks balanced parentheses correctly', () => {
		expect(hasBalancedParens('foo.ts')).toBe(true);
		expect(hasBalancedParens('file(1).png')).toBe(true);
		expect(hasBalancedParens('dir(a)/file(b).ts')).toBe(true);
		expect(hasBalancedParens('file(1.png')).toBe(false);
		expect(hasBalancedParens('file)1(.png')).toBe(false);
	});

	it('detects valid file paths with looksLikePath', () => {
		expect(looksLikePath('src/terminal/index.ts')).toBe(true);
		expect(looksLikePath('src/terminal')).toBe(true);
		expect(looksLikePath('image(1).png')).toBe(true);
		expect(looksLikePath('README.md')).toBe(true);
		expect(looksLikePath('e.g.')).toBe(false);
		expect(looksLikePath('Mr.A')).toBe(false);
		expect(looksLikePath('//bad/path')).toBe(false);
	});

	it('normalizes path segments without filesystem operations', () => {
		expect(normalizePath('src/./terminal/../terminal/index.ts')).toBe('src/terminal/index.ts');
		expect(normalizePath('/foo/bar/../baz')).toBe('/foo/baz');
		expect(normalizePath('C:/Users/../Users/test')).toBe('C:/Users/test');
	});

	it('resolves path candidates asynchronously with fs validation (T-02)', async () => {
		const mockFs = async (p: string) => p === '/workspace/src/real.ts';
		const res = await resolvePathCandidates('src/real.ts', '/workspace', mockFs);
		expect(res).toBe('/workspace/src/real.ts');

		const fallback = await resolvePathCandidates('src/missing.ts', '/workspace', mockFs);
		expect(fallback).toBe('/workspace/src/missing.ts');
	});
});
