import { describe, expect, it } from 'vitest';
import {
	fileUrlToPath,
	hasBalancedParens,
	looksLikePath,
	normalizePath,
	resolveExistingPath,
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

describe('resolveExistingPath', () => {
	const onDisk = new Set(['/repo/src/index.ts', '/repo/src/terminal']);
	const fsCheck = async (p: string) => onDisk.has(p);

	it('returns the resolved path when it exists', async () => {
		expect(await resolveExistingPath('src/index.ts', '/repo', fsCheck)).toBe('/repo/src/index.ts');
		expect(await resolveExistingPath('src/terminal', '/repo', fsCheck)).toBe('/repo/src/terminal');
	});

	it('returns null for path-shaped prose that is not on disk', async () => {
		for (const tok of ['24/7', 'and/or', 'n/a', 'km/h', 'he/him']) {
			expect(await resolveExistingPath(tok, '/repo', fsCheck)).toBeNull();
		}
	});

	it('returns null when no fs check is supplied', async () => {
		expect(await resolveExistingPath('src/index.ts', '/repo')).toBeNull();
	});

	it('treats a failing fs check as non-existent', async () => {
		const boom = async () => {
			throw new Error('EACCES');
		};
		expect(await resolveExistingPath('src/index.ts', '/repo', boom)).toBeNull();
	});
});

describe('fileUrlToPath', () => {
	it('strips the authority from a hosted file URL', () => {
		expect(fileUrlToPath('file://hostname/home/u/proj')).toBe('/home/u/proj');
		expect(fileUrlToPath('file:///home/u/proj')).toBe('/home/u/proj');
	});

	it('drops the leading slash before a Windows drive letter', () => {
		// The regression: a Windows shell's OSC 7 emits file:///C:/… and the
		// naive parse leaves /C:/… , which resolves against nothing.
		expect(fileUrlToPath('file:///C:/Users/ned/proj')).toBe('C:/Users/ned/proj');
		expect(fileUrlToPath('file:///d:/work')).toBe('d:/work');
	});

	it('percent-decodes the path', () => {
		expect(fileUrlToPath('file:///home/u/my%20docs')).toBe('/home/u/my docs');
		expect(fileUrlToPath('file:///C:/Program%20Files/x')).toBe('C:/Program Files/x');
	});

	it('returns non-file input unchanged', () => {
		expect(fileUrlToPath('/already/a/path')).toBe('/already/a/path');
		expect(fileUrlToPath('C:/win/path')).toBe('C:/win/path');
		expect(fileUrlToPath('  /trimmed  ')).toBe('/trimmed');
	});

	it('falls back to the raw path on malformed percent-encoding', () => {
		expect(fileUrlToPath('file:///bad/%E0%A4%A')).toBe('/bad/%E0%A4%A');
	});
});
