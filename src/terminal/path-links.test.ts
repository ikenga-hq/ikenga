import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fsExists } from '@/lib/tauri-cmd';
import {
	__clearPathLinkCache,
	offsetToCell,
	readLogicalLine,
	registerPathLinks,
	scanLineForPaths,
} from './path-links';

vi.mock('@/lib/tauri-cmd', () => ({ fsExists: vi.fn() }));
vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: { getState: () => ({ focusedId: 'p1', addTabBackground: vi.fn() }) },
}));

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

// Tokens that exist on disk in this fake fs. Everything else resolves to
// nothing, which is what separates a real relative dir from prose.
const REAL = new Set(['/repo/src/index.ts', '/repo/src/terminal']);

function fakeTerm(lineText: string) {
	let provider: {
		provideLinks: (y: number, cb: (links: unknown[] | undefined) => void) => void;
	} | null = null;
	return {
		buffer: { active: { getLine: () => ({ translateToString: () => lineText }) } },
		registerLinkProvider(p: typeof provider) {
			provider = p;
			return { dispose() {} };
		},
		getLinks(): Promise<{ text: string }[]> {
			return new Promise((resolve) => {
				provider?.provideLinks(1, (links) => resolve((links ?? []) as { text: string }[]));
			});
		},
	};
}

describe('registerPathLinks — decorates only what exists on disk', () => {
	beforeEach(() => {
		__clearPathLinkCache();
		vi.mocked(fsExists).mockImplementation(async (p: string) => REAL.has(p));
	});

	it('does not linkify prose that is merely path-shaped', async () => {
		const term = fakeTerm('available 24/7 for read/write and/or n/a see src/index.ts');
		registerPathLinks(term as never, '/repo');
		const links = await term.getLinks();
		// scanLineForPaths still matches all five; only the real file survives.
		expect(links.map((l) => l.text)).toEqual(['src/index.ts']);
	});

	it('still linkifies a bare relative directory that exists', async () => {
		const term = fakeTerm('cd src/terminal to continue');
		registerPathLinks(term as never, '/repo');
		const links = await term.getLinks();
		expect(links.map((l) => l.text)).toEqual(['src/terminal']);
	});

	it('emits no links when nothing on the line exists', async () => {
		const term = fakeTerm('ratio is 24/7 and km/h he/him');
		registerPathLinks(term as never, '/repo');
		const links = await term.getLinks();
		expect(links).toEqual([]);
	});

	it('memoizes existence checks across repeat hovers', async () => {
		const term = fakeTerm('see src/index.ts');
		registerPathLinks(term as never, '/repo');
		await term.getLinks();
		const afterFirst = vi.mocked(fsExists).mock.calls.length;
		expect(afterFirst).toBeGreaterThan(0); // guard: the mock is actually exercised
		await term.getLinks();
		expect(vi.mocked(fsExists).mock.calls.length).toBe(afterFirst);
	});
});

// ── Wrapped rows ────────────────────────────────────────────────────────────
//
// xterm splits a logical row wider than the terminal into several physical
// buffer lines, flagging each continuation with `isWrapped`. Scanning one
// physical line at a time missed exactly the paths most worth clicking: a long
// absolute path in a narrow split, whose halves are not path-shaped alone.

/** Fake buffer of physical rows, each `cols` wide, wrapped as marked. */
function wrappedTerm(rows: { text: string; isWrapped: boolean }[], cols: number) {
	let provider: {
		provideLinks: (y: number, cb: (links: unknown[] | undefined) => void) => void;
	} | null = null;
	return {
		cols,
		buffer: {
			active: {
				getLine: (i: number) => {
					const r = rows[i];
					if (!r) return undefined;
					return {
						isWrapped: r.isWrapped,
						// Untrimmed rows are padded to full width, as xterm does.
						translateToString: (trim: boolean) =>
							trim ? r.text.replace(/\s+$/, '') : r.text.padEnd(cols, ' '),
					};
				},
			},
		},
		registerLinkProvider(p: typeof provider) {
			provider = p;
			return { dispose() {} };
		},
		getLinks(y: number): Promise<{ text: string; range: unknown }[]> {
			return new Promise((resolve) => {
				provider?.provideLinks(y, (links) =>
					resolve((links ?? []) as { text: string; range: unknown }[])
				);
			});
		},
	};
}

describe('readLogicalLine — rejoins wrapped rows', () => {
	it('joins forward from the first row of a wrapped run', () => {
		const term = wrappedTerm(
			[
				{ text: 'see /repo/sr', isWrapped: false },
				{ text: 'c/index.ts n', isWrapped: true },
				{ text: 'ow', isWrapped: true },
			],
			12
		);
		const logical = readLogicalLine(term as never, 1);
		expect(logical?.startLine).toBe(1);
		expect(logical?.text).toBe('see /repo/src/index.ts now');
	});

	it('walks BACK to the logical start when handed a continuation row', () => {
		const term = wrappedTerm(
			[
				{ text: 'see /repo/sr', isWrapped: false },
				{ text: 'c/index.ts n', isWrapped: true },
				{ text: 'ow', isWrapped: true },
			],
			12
		);
		// xterm calls provideLinks for every row of the run, including row 2.
		const logical = readLogicalLine(term as never, 2);
		expect(logical?.startLine).toBe(1);
		expect(logical?.text).toBe('see /repo/src/index.ts now');
	});

	it('does not swallow the following unwrapped row', () => {
		const term = wrappedTerm(
			[
				{ text: 'aaaaaaaaaaaa', isWrapped: false },
				{ text: 'bbb', isWrapped: true },
				{ text: 'a separate line', isWrapped: false },
			],
			12
		);
		expect(readLogicalLine(term as never, 1)?.text).toBe('aaaaaaaaaaaabbb');
	});
});

describe('offsetToCell', () => {
	it('maps an offset past the wrap onto the next row', () => {
		const logical = { text: 'x'.repeat(30), startLine: 5, cols: 12 };
		expect(offsetToCell(1, logical)).toEqual({ x: 1, y: 5 });
		expect(offsetToCell(12, logical)).toEqual({ x: 12, y: 5 });
		expect(offsetToCell(13, logical)).toEqual({ x: 1, y: 6 });
		expect(offsetToCell(25, logical)).toEqual({ x: 1, y: 7 });
	});
});

describe('registerPathLinks — wrapped paths', () => {
	beforeEach(() => {
		__clearPathLinkCache();
		vi.mocked(fsExists).mockImplementation(async (p: string) => REAL.has(p));
	});

	it('linkifies a path split across a wrap, with a range spanning both rows', async () => {
		const term = wrappedTerm(
			[
				{ text: 'see /repo/sr', isWrapped: false },
				{ text: 'c/index.ts', isWrapped: true },
			],
			12
		);
		registerPathLinks(term as never, '/repo');
		const links = await term.getLinks(1);
		expect(links.map((l) => l.text)).toEqual(['/repo/src/index.ts']);
		// '/repo/src/index.ts' starts at offset 5 -> row 1 col 5, and ends at
		// offset 22 -> row 2 col 10. A single-line scan could never produce this.
		expect(links[0].range).toEqual({ start: { x: 5, y: 1 }, end: { x: 10, y: 2 } });
	});

	it('returns the same link when asked about the continuation row', async () => {
		const term = wrappedTerm(
			[
				{ text: 'see /repo/sr', isWrapped: false },
				{ text: 'c/index.ts', isWrapped: true },
			],
			12
		);
		registerPathLinks(term as never, '/repo');
		const links = await term.getLinks(2);
		expect(links.map((l) => l.text)).toEqual(['/repo/src/index.ts']);
	});

	it('proves the regression: each half alone is not path-shaped', async () => {
		// Guard against a future "optimisation" that drops the rejoin.
		expect(scanLineForPaths('see /repo/sr').map((s) => s.text)).toEqual(['/repo/sr']);
		const term = wrappedTerm([{ text: 'see /repo/sr', isWrapped: false }], 12);
		registerPathLinks(term as never, '/repo');
		// '/repo/sr' is not on disk, so the truncated half yields nothing.
		expect(await term.getLinks(1)).toEqual([]);
	});
});

describe('scanLineForPaths — the wall-clock budget must not eat ordinary lines', () => {
	it('still finds paths when the budget is already blown on entry', () => {
		// Simulates a descheduled process: every clock read is far past any
		// deadline. Before the token threshold this returned [] for a 43-char
		// line, which is how two tests failed only under full-suite parallelism.
		const spy = vi.spyOn(performance, 'now').mockReturnValue(1e12);
		try {
			const spans = scanLineForPaths('› [image] /tmp/v1-list-detail.png (145.3KB)');
			expect(spans.map((s) => s.text)).toEqual(['/tmp/v1-list-detail.png']);
		} finally {
			spy.mockRestore();
		}
	});

	it('still bails out on pathological input', () => {
		let calls = 0;
		const spy = vi.spyOn(performance, 'now').mockImplementation(() => {
			calls++;
			return calls === 1 ? 0 : 1e12;
		});
		try {
			// 2000 tokens, well past the 64-token grace — the guard must engage.
			const spans = scanLineForPaths(Array(2000).fill('a/b.ts').join(' '));
			expect(spans.length).toBeLessThan(2000);
		} finally {
			spy.mockRestore();
		}
	});
});
