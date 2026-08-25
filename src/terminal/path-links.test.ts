import { describe, expect, it } from 'vitest';
import { scanLineForPaths } from './path-links';

describe('scanLineForPaths', () => {
	it('finds an absolute path with a known extension', () => {
		const line = '› [image] /tmp/v1-list-detail.png (145.3KB)';
		const spans = scanLineForPaths(line);
		expect(spans).toHaveLength(1);
		expect(spans[0].text).toBe('/tmp/v1-list-detail.png');
		// 1-based inclusive cell columns
		const start = line.indexOf('/tmp');
		expect(spans[0].startX).toBe(start + 1);
		expect(spans[0].endX).toBe(start + '/tmp/v1-list-detail.png'.length);
	});

	it('finds a relative source path', () => {
		const spans = scanLineForPaths('edited src/foo/bar.ts and src/baz.tsx');
		expect(spans.map((s) => s.text)).toEqual(['src/foo/bar.ts', 'src/baz.tsx']);
	});

	it('strips surrounding parens and trailing punctuation', () => {
		const spans = scanLineForPaths('see (/tmp/out.png), then done.');
		expect(spans).toHaveLength(1);
		expect(spans[0].text).toBe('/tmp/out.png');
	});

	it('strips and extracts a :line:col suffix', () => {
		const spans = scanLineForPaths('  at src/index.ts:42:7');
		expect(spans).toHaveLength(1);
		expect(spans[0].text).toBe('src/index.ts');
		expect(spans[0].line).toBe(42);
		expect(spans[0].col).toBe(7);
	});

	it('strips and extracts a :line suffix', () => {
		const spans = scanLineForPaths('file src/index.ts:105');
		expect(spans).toHaveLength(1);
		expect(spans[0].text).toBe('src/index.ts');
		expect(spans[0].line).toBe(105);
		expect(spans[0].col).toBeUndefined();
	});

	it('strips and extracts a (line, col) suffix', () => {
		const spans = scanLineForPaths('error in src/index.ts(23,4)');
		expect(spans).toHaveLength(1);
		expect(spans[0].text).toBe('src/index.ts');
		expect(spans[0].line).toBe(23);
		expect(spans[0].col).toBe(4);
	});

	it('ignores URLs and prose', () => {
		expect(scanLineForPaths('visit https://example.com/x.png now')).toHaveLength(0);
		expect(scanLineForPaths('e.g. this is fine, Mr.A')).toHaveLength(0);
	});

	it('ignores a single-segment token with an unknown extension', () => {
		expect(scanLineForPaths('build finished in 1.234s')).toHaveLength(0);
	});

	// Regression: `PATH_RE` used a single `[~/]?` head, which consumed the `~` of
	// `~/foo.md` and then required `[\w.@]` to match the `/`. Every `~/` path
	// failed detection even though `resolvePath` had a working expansion branch
	// for it, so that branch was unreachable.
	it('finds a ~/-rooted path', () => {
		const spans = scanLineForPaths('wrote ~/royalti-co/.company/plan.md ok');
		expect(spans.map((s) => s.text)).toEqual(['~/royalti-co/.company/plan.md']);
	});

	it('finds a ~user-rooted path', () => {
		const spans = scanLineForPaths('see ~nedjamez/notes.md');
		expect(spans.map((s) => s.text)).toEqual(['~nedjamez/notes.md']);
	});

	it('reports correct columns for a ~/ path', () => {
		const line = 'wrote ~/a/b.md done';
		const spans = scanLineForPaths(line);
		expect(spans).toHaveLength(1);
		const start = line.indexOf('~/a/b.md');
		expect(spans[0].startX).toBe(start + 1);
		expect(spans[0].endX).toBe(start + '~/a/b.md'.length);
	});

	it('still rejects malformed tilde/slash heads', () => {
		expect(scanLineForPaths('check //foo.md and ~~/bar.md')).toHaveLength(0);
	});

	it('finds directory paths containing slashes without extensions', () => {
		const spans = scanLineForPaths('explore src/terminal or ~/.claude/projects or ./dist');
		expect(spans.map((s) => s.text)).toEqual(['src/terminal', '~/.claude/projects', './dist']);
	});

	it('preserves balanced parentheses in filenames (T-17)', () => {
		const spans = scanLineForPaths('generated /tmp/report(1).pdf and image(final).png');
		expect(spans.map((s) => s.text)).toEqual(['/tmp/report(1).pdf', 'image(final).png']);
	});

	it('handles long lines safely within execution budget', () => {
		const longLine = 'see src/index.ts ' + 'x'.repeat(3000);
		const spans = scanLineForPaths(longLine);
		expect(spans).toHaveLength(1);
		expect(spans[0].text).toBe('src/index.ts');
	});
});
