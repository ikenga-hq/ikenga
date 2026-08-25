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
import { fsExists } from '@/lib/tauri-cmd';
import { hasBalancedParens, looksLikePath, resolvePathCandidates } from '@/lib/paths/file-paths';
import { usePaneStore } from '@/lib/panes/pane-store';

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

/**
 * Register the file-path link provider on a terminal. Returns a disposable;
 * call it from the host's cleanup. `cwd` resolves relative paths (absolute /
 * `~` paths ignore it). Can be a static string or a dynamic getter function.
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
			const links: ILink[] = spans.map((span) => ({
				text: span.text,
				range: {
					start: { x: span.startX, y: bufferLineNumber },
					end: { x: span.endX, y: bufferLineNumber },
				},
				decorations: { pointerCursor: true, underline: true },
				activate: () => {
					const effectiveCwd = typeof cwd === 'function' ? cwd() : cwd;
					resolvePathCandidates(span.text, effectiveCwd, fsExists).then((resolved) => {
						const store = usePaneStore.getState();
						store.addTabBackground(store.focusedId, {
							kind: 'artifact',
							path: resolved,
							line: span.line,
							col: span.col,
						});
					});
				},
			}));
			callback(links);
		},
	});
}
