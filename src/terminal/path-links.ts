/**
 * xterm link provider for file paths.
 *
 * `WebLinksAddon` only linkifies `http(s)://` URLs. This provider adds clickable
 * *file paths* — so a `/tmp/out.png` or `src/foo.ts` printed by a command opens
 * in the artifact viewer pane, the same destination a path pill opens from a
 * rendered document.
 *
 * Detection is shared with the markdown renderer via `looksLikePath`
 * (`@/lib/paths/file-paths`) so the two surfaces never disagree about what's a
 * path. Resolution is the synchronous `resolvePath` (expand `~`, join relative
 * against the terminal's cwd); absolute paths — the common terminal case — pass
 * through untouched, so this works even in attach-mode where no cwd is known.
 *
 * Wrapped paths are handled. xterm stores a logical row that exceeds the
 * terminal width as several physical buffer lines, with `isWrapped` set on each
 * continuation. Scanning one physical line at a time therefore missed exactly
 * the paths most worth clicking — a long absolute path in a narrow split is
 * split across rows, and each half is not path-shaped on its own. We rejoin the
 * logical row first, scan that, then map offsets back to (row, column). An
 * `ILink.range` may legitimately span rows; xterm renders the underline across
 * them.
 *
 * Known limitation: column ranges are computed against the cell string, so a
 * line containing wide (CJK/emoji) glyphs *before* a path can offset the
 * underline by a cell. Paths themselves are ASCII, so the link text and click
 * target are always correct — only the highlight rectangle can drift.
 */

import type { IDisposable, ILink, Terminal } from '@xterm/xterm';
import { usePaneStore } from '@/lib/panes/pane-store';
import { hasBalancedParens, looksLikePath, resolveExistingPath } from '@/lib/paths/file-paths';
import { fsExists } from '@/lib/tauri-cmd';

export interface PathSpan {
	/** 1-based start column (inclusive). */
	startX: number;
	/** 1-based end column (inclusive). */
	endX: number;
	text: string;
	line?: number;
	col?: number;
}

/** Find path-shaped tokens in one rendered line, with their cell columns.
 *  Exported for unit testing. Includes wall-clock cutoff and balanced paren handling (T-17). */
const DEADLINE_CHECK_AFTER_TOKENS = 64;

export function scanLineForPaths(line: string): PathSpan[] {
	if (!line) return [];
	const out: PathSpan[] = [];
	// Cap line length to 2048 to prevent pathological regex stalls
	const safeLine = line.length > 2048 ? line.slice(0, 2048) : line;
	const deadline = performance.now() + 5; // 5ms maximum budget per line

	const re = /\S+/g;
	let m: RegExpExecArray | null;
	let tokensSeen = 0;
	// biome-ignore lint/suspicious/noAssignInExpressions: standard exec loop
	while ((m = re.exec(safeLine)) !== null) {
		// The wall-clock budget guards pathological input, so it must not be
		// consultable until the input is plausibly pathological. Checking it on
		// token 1 made the function nondeterministic: if the process is
		// descheduled between setting `deadline` and the first iteration — which
		// happens routinely under a loaded machine, and reproducibly when the
		// full test suite runs files in parallel — the loop breaks immediately
		// and a perfectly ordinary line yields zero paths. On a busy desktop that
		// is a link provider that silently stops linking. No real line has 64
		// whitespace-separated tokens before its first path.
		if (tokensSeen++ >= DEADLINE_CHECK_AFTER_TOKENS && performance.now() > deadline) break;

		let tok = m[0];
		let start = m.index; // 0-based offset of first char

		// Strip surrounding wrappers/punctuation a path is often embedded in:
		// `(…)`, `[…]`, `<…>`, quotes, and trailing sentence punctuation.
		while (tok.length > 0 && /^[([<'"`]/.test(tok)) {
			// If token has balanced parens (e.g. `(file.txt)`), stripping `(` requires stripping `)` later
			tok = tok.slice(1);
			start++;
		}
		let end = start + tok.length; // 0-based, exclusive

		let lineNum: number | undefined;
		let colNum: number | undefined;

		// 1. Trim paren line/col suffix e.g. `foo.ts(42,7)` or `foo.ts(42)`
		const parenMatch = tok.match(/\((\d+)(?:,\s*(\d+))?\)$/);
		if (parenMatch && looksLikePath(tok.slice(0, tok.length - parenMatch[0].length))) {
			const cut = parenMatch[0].length;
			lineNum = parseInt(parenMatch[1], 10);
			if (parenMatch[2]) colNum = parseInt(parenMatch[2], 10);
			tok = tok.slice(0, tok.length - cut);
			end -= cut;
		}

		// 2. Trim a trailing `:line` / `:line:col` suffix (grep -n, stack traces)
		// if doing so leaves a real path.
		const colon = tok.match(/:(\d+)(?::(\d+))?$/);
		if (colon && looksLikePath(tok.slice(0, tok.length - colon[0].length))) {
			const cut = colon[0].length;
			lineNum = parseInt(colon[1], 10);
			if (colon[2]) colNum = parseInt(colon[2], 10);
			tok = tok.slice(0, tok.length - cut);
			end -= cut;
		}

		// Drop trailing closers/punctuation until the token is path-shaped,
		// but preserve trailing ')' if parentheses are already balanced internally (e.g. `file(1).png`).
		while (tok.length >= 2 && !looksLikePath(tok)) {
			const last = tok[tok.length - 1];
			if (last === ')' && hasBalancedParens(tok)) {
				break;
			}
			if (/[)\]>'"`.,;:]$/.test(tok)) {
				tok = tok.slice(0, -1);
				end--;
			} else {
				break;
			}
		}

		if (tok.length >= 2 && looksLikePath(tok)) {
			// cell columns are 1-based; char at offset `start` is column start+1,
			// and the last char (offset end-1) is column `end`.
			out.push({
				startX: start + 1,
				endX: end,
				text: tok,
				line: lineNum,
				col: colNum,
			});
		}
	}
	return out;
}

// Memoize existence checks so re-hovering a line doesn't refire IPC per token.
// Bounded to 500 entries to prevent unbounded growth over long sessions.
// Keyed by (cwd, rawPath). Mirrors the cache in components/markdown.tsx.
const MAX_EXISTS_CACHE_SIZE = 500;
const existsCache = new Map<string, Promise<string | null>>();

function resolveExistingCached(rawPath: string, cwd: string | undefined): Promise<string | null> {
	const key = `${cwd ?? ''}|${rawPath}`;
	let cached = existsCache.get(key);
	if (!cached) {
		if (existsCache.size >= MAX_EXISTS_CACHE_SIZE) {
			const oldestKey = existsCache.keys().next().value;
			if (oldestKey !== undefined) existsCache.delete(oldestKey);
		}
		cached = resolveExistingPath(rawPath, cwd, fsExists);
		existsCache.set(key, cached);
	}
	return cached;
}

/** Test seam — drops memoized existence results. */
export function __clearPathLinkCache(): void {
	existsCache.clear();
}

/**
 * Cap on how many physical rows we will rejoin into one logical row.
 *
 * A pasted blob can wrap over hundreds of rows, and `provideLinks` is called
 * once per row — rejoining and rescanning the whole thing each time would be
 * quadratic in the size of the paste. Sixteen rows is ~1900 columns at a
 * typical width, comfortably more than any real path, and bounds the work.
 */
const MAX_WRAPPED_ROWS = 16;

export interface LogicalLine {
	/** The rejoined text of the whole logical row. */
	text: string;
	/** 1-based buffer line number the logical row starts on. */
	startLine: number;
	/** Columns per physical row — the modulus for mapping offsets back. */
	cols: number;
}

/**
 * Rejoin the logical row that `bufferLineNumber` belongs to.
 *
 * Uses `translateToString(false)` — untrimmed, padded to the full width — for
 * every segment, because trimming would swallow the cells a wrapped path
 * continues across and break the offset arithmetic. Trailing pad on the final
 * row is harmless: it yields no tokens.
 *
 * Degrades to today's single-line behaviour when the buffer exposes no
 * `isWrapped` (or no width), so a terminal that never wraps is unaffected.
 *
 * Exported for unit testing.
 */
export function readLogicalLine(term: Terminal, bufferLineNumber: number): LogicalLine | null {
	const buf = term.buffer.active;
	const at = (n: number) => (n >= 1 ? buf.getLine(n - 1) : undefined);

	const here = at(bufferLineNumber);
	if (!here) return null;

	// Walk back to the first physical row of this logical row. `isWrapped` on
	// line N means "N continues N-1", so keep stepping while the CURRENT line
	// is a continuation.
	let start = bufferLineNumber;
	while (start > 1 && at(start)?.isWrapped && bufferLineNumber - start < MAX_WRAPPED_ROWS) {
		start--;
	}

	// Walk forward while the NEXT line is a continuation of this one.
	let end = start;
	while (end - start + 1 < MAX_WRAPPED_ROWS && at(end + 1)?.isWrapped) {
		end++;
	}

	const segments: string[] = [];
	for (let n = start; n <= end; n++) {
		const line = at(n);
		if (!line) break;
		// Untrimmed for every row but the last, so column math stays exact.
		segments.push(line.translateToString(n === end));
	}
	const text = segments.join('');
	// `term.cols` is the modulus for offset -> (row, column). Fall back to the
	// whole length when it is unavailable, which collapses to one row.
	const cols = term.cols && term.cols > 0 ? term.cols : Math.max(text.length, 1);
	return { text, startLine: start, cols };
}

/** Map a 1-based offset in the rejoined logical row back to a buffer cell. */
export function offsetToCell(offset1: number, logical: LogicalLine): { x: number; y: number } {
	const zero = offset1 - 1;
	return {
		x: (zero % logical.cols) + 1,
		y: logical.startLine + Math.floor(zero / logical.cols),
	};
}

/**
 * Register the file-path link provider on a terminal. Returns a disposable;
 * call it from the host's cleanup. `cwd` resolves relative paths (absolute /
 * `~` paths ignore it). Can be a static string or a dynamic getter function.
 *
 * Only tokens that actually resolve to something on disk are decorated.
 * `scanLineForPaths` accepts any multi-segment token so bare directories
 * (`src/terminal`) linkify, but that also matches prose — `24/7`, `and/or`,
 * `n/a`, `km/h`. Those are lexically indistinguishable from a real relative
 * directory, so existence is the discriminator. The resolved path is carried
 * onto the link, so `activate` opens it without re-resolving.
 */
export function registerPathLinks(
	term: Terminal,
	cwd?: string | (() => string | undefined)
): IDisposable {
	return term.registerLinkProvider({
		provideLinks(bufferLineNumber: number, callback: (links: ILink[] | undefined) => void) {
			const logical = readLogicalLine(term, bufferLineNumber);
			if (!logical) {
				callback(undefined);
				return;
			}
			const spans = scanLineForPaths(logical.text);
			if (spans.length === 0) {
				callback(undefined);
				return;
			}
			const effectiveCwd = typeof cwd === 'function' ? cwd() : cwd;
			Promise.all(spans.map((span) => resolveExistingCached(span.text, effectiveCwd)))
				.then((resolved) => {
					const links: ILink[] = [];
					spans.forEach((span, idx) => {
						const path = resolved[idx];
						if (!path) return; // not on disk — prose, not a path
						links.push({
							text: span.text,
							// May span rows when the path wraps — that is the point.
							range: {
								start: offsetToCell(span.startX, logical),
								end: offsetToCell(span.endX, logical),
							},
							decorations: { pointerCursor: true, underline: true },
							activate: () => {
								const store = usePaneStore.getState();
								store.addTabBackground(store.focusedId, {
									kind: 'artifact',
									path,
									line: span.line,
									col: span.col,
								});
							},
						});
					});
					callback(links.length > 0 ? links : undefined);
				})
				.catch(() => callback(undefined));
		},
	});
}
