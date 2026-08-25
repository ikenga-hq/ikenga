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
export function scanLineForPaths(line: string): PathSpan[] {
	if (!line) return [];
	const out: PathSpan[] = [];
	// Cap line length to 2048 to prevent pathological regex stalls
	const safeLine = line.length > 2048 ? line.slice(0, 2048) : line;
	const deadline = performance.now() + 5; // 5ms maximum budget per line

	const re = /\S+/g;
	let m: RegExpExecArray | null;
	// biome-ignore lint/suspicious/noAssignInExpressions: standard exec loop
	while ((m = re.exec(safeLine)) !== null) {
		if (performance.now() > deadline) break;

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
			const line = term.buffer.active.getLine(bufferLineNumber - 1);
			if (!line) {
				callback(undefined);
				return;
			}
			const text = line.translateToString(true);
			const spans = scanLineForPaths(text);
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
							range: {
								start: { x: span.startX, y: bufferLineNumber },
								end: { x: span.endX, y: bufferLineNumber },
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
