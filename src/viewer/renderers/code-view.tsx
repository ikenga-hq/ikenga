import { useEffect, useRef, useState } from 'react';
import { AlertCircle, Loader2 } from 'lucide-react';
import { useTheme } from '@/lib/theme';
import { fsRead } from '@/lib/tauri-cmd';
import { detectLang } from '../lib/lang';

interface CodeViewProps {
	path: string;
	line?: number;
	col?: number;
}

// Shiki is heavy — load lazily on first mount and cache the highlighter
// instance across renders so reopening files doesn't reinitialize WASM.
let highlighterPromise: Promise<typeof import('shiki')> | null = null;
function loadShiki() {
	if (!highlighterPromise) {
		highlighterPromise = import('shiki');
	}
	return highlighterPromise;
}

export function CodeView({ path, line, col }: CodeViewProps) {
	const { resolvedTheme } = useTheme();
	const isDark = resolvedTheme === 'dark';
	const containerRef = useRef<HTMLDivElement | null>(null);
	const [state, setState] = useState<
		| { kind: 'loading' }
		| { kind: 'ready'; html: string; raw: string; lang: string }
		| { kind: 'error'; message: string }
	>({ kind: 'loading' });

	useEffect(() => {
		let cancelled = false;
		setState({ kind: 'loading' });

		(async () => {
			try {
				const res = await fsRead(path);
				if (cancelled) return;
				const text = new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(res.bytes));
				const lang = detectLang(path);
				const shiki = await loadShiki();
				const html = await shiki.codeToHtml(text, {
					lang: (lang as never) ?? 'text',
					theme: isDark ? 'github-dark' : 'github-light',
				});
				if (cancelled) return;
				setState({ kind: 'ready', html, raw: text, lang });
			} catch (err) {
				if (cancelled) return;
				setState({
					kind: 'error',
					message: err instanceof Error ? err.message : String(err),
				});
			}
		})();

		return () => {
			cancelled = true;
		};
	}, [path, isDark]);

	useEffect(() => {
		if (state.kind !== 'ready' || !line || !containerRef.current) return;

		// Clear previous line highlights and cursor carets
		const prevLines = containerRef.current.querySelectorAll('.line');
		prevLines.forEach((l) => {
			(l as HTMLElement).style.backgroundColor = '';
		});
		const prevCarets = containerRef.current.querySelectorAll('.code-cursor-caret');
		prevCarets.forEach((el) => el.remove());

		const lines = containerRef.current.querySelectorAll('.line');
		const target = lines[line - 1] as HTMLElement | undefined;
		if (target) {
			target.scrollIntoView({ block: 'center', behavior: 'smooth' });
			target.style.backgroundColor = isDark ? 'rgba(255, 255, 0, 0.15)' : 'rgba(255, 255, 0, 0.25)';
			target.style.borderRadius = '2px';
			target.style.display = 'inline-block';
			target.style.width = '100%';

			if (col && col > 0) {
				// Locate character position at 1-based column `col` (T-04)
				let curCol = 1;
				let foundNode: Node | null = null;
				let foundOffset = 0;
				const walker = document.createTreeWalker(target, NodeFilter.SHOW_TEXT);
				let n = walker.nextNode();
				while (n) {
					const len = n.nodeValue?.length ?? 0;
					if (curCol + len >= col) {
						foundNode = n;
						foundOffset = Math.max(0, Math.min(col - curCol, len));
						break;
					}
					curCol += len;
					n = walker.nextNode();
				}

				if (foundNode) {
					try {
						const range = document.createRange();
						range.setStart(foundNode, foundOffset);
						range.collapse(true);
						const sel = window.getSelection();
						if (sel) {
							sel.removeAllRanges();
							sel.addRange(range);
						}
					} catch {}

					try {
						const caret = document.createElement('span');
						caret.className =
							'code-cursor-caret inline-block w-[2px] h-[1.15em] bg-primary animate-pulse align-middle -ml-[1px] relative z-10';
						caret.setAttribute('data-col', String(col));
						const textNode = foundNode as Text;
						if (foundOffset === 0) {
							textNode.parentNode?.insertBefore(caret, textNode);
						} else if (foundOffset >= textNode.length) {
							textNode.parentNode?.insertBefore(caret, textNode.nextSibling);
						} else {
							const after = textNode.splitText(foundOffset);
							textNode.parentNode?.insertBefore(caret, after);
						}
					} catch {}
				}
			}
		} else {
			const approxLineHeight = 18;
			containerRef.current.scrollTop = Math.max(0, (line - 5) * approxLineHeight);
		}
	}, [state.kind, line, col, isDark]);

	if (state.kind === 'loading') {
		return (
			<div className="flex h-full items-center justify-center text-xs text-muted-foreground">
				<Loader2 className="mr-2 h-4 w-4 animate-spin" /> Loading…
			</div>
		);
	}
	if (state.kind === 'error') {
		// Shiki throws if the language grammar is missing. Fall back to a plain
		// <pre> render so the user still sees the file.
		return <FallbackPre path={path} message={state.message} line={line} col={col} />;
	}
	return (
		<div
			ref={containerRef}
			className="h-full overflow-auto bg-background px-4 py-3 text-xs leading-relaxed [&_pre]:!bg-transparent [&_pre]:m-0"
			// shiki produces self-contained HTML with inline colors — safe to inject
			// because the source comes from our allowlisted fs_read.
			dangerouslySetInnerHTML={{ __html: state.html }}
		/>
	);
}

function FallbackPre({
	path,
	message,
	line,
	col: _col,
}: { path: string; message: string; line?: number; col?: number }) {
	const [text, setText] = useState<string | null>(null);
	const preRef = useRef<HTMLPreElement | null>(null);
	useEffect(() => {
		let cancelled = false;
		fsRead(path)
			.then((res) => {
				if (cancelled) return;
				setText(new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(res.bytes)));
			})
			.catch(() => {});
		return () => {
			cancelled = true;
		};
	}, [path]);

	useEffect(() => {
		if (!line || !preRef.current) return;
		const approxLineHeight = 18;
		preRef.current.scrollTop = Math.max(0, (line - 5) * approxLineHeight);
	}, [line, text]);
	return (
		<div className="flex h-full flex-col">
			<div className="flex shrink-0 items-center gap-2 border-b border-border bg-amber-500/10 px-4 py-2 text-xs text-amber-600 dark:text-amber-400">
				<AlertCircle className="h-3.5 w-3.5" />
				<span>Highlighter failed: {message}. Showing raw text.</span>
			</div>
			<pre className="m-0 flex-1 overflow-auto px-4 py-3 font-mono text-xs leading-relaxed text-foreground">
				{text ?? ''}
			</pre>
		</div>
	);
}
