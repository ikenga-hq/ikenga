/**
 * Shared markdown renderer. Uses react-markdown + remark-gfm + shiki for
 * syntax-highlighted code blocks with a click-to-copy button. File-shaped
 * paths get auto-linkified to open in the artifact viewer pane.
 *
 * (Streamdown was tried for streaming-aware partial-fence handling but its
 * built-in code dispatch refused to yield to `components.code` /
 * `plugins.renderers`. Reverted to react-markdown for now; can revisit per
 * surface later.)
 */

import { useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import { Check, Copy, FileText } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import rehypeRaw from 'rehype-raw';
import rehypeSlug from 'rehype-slug';
import rehypeAutolinkHeadings from 'rehype-autolink-headings';
// `unified`'s types ship transitively via react-markdown; the previous
// direct import broke once unified left node_modules. Use the
// react-markdown type instead so we stay compatible with whatever
// version comes through.
type PluggableList = NonNullable<React.ComponentProps<typeof ReactMarkdown>['rehypePlugins']>;
import { useTheme } from '@/lib/theme';
import { loadHome } from '@/lib/home';
import { usePaneStore } from '@/lib/panes/pane-store';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { fsExists } from '@/lib/tauri-cmd';
import { looksLikePath, resolvePath } from '@/lib/paths/file-paths';
import { cn } from '@/components/ui/utils';

interface MarkdownProps {
	content: string;
	className?: string;
	/** Working dir to resolve relative paths against. */
	cwd?: string;
	/** Allow embedded HTML (file viewer use case). Off by default for safety. */
	allowHtml?: boolean;
	/**
	 * `comfortable` (default) — doc viewer: GitHub-style large headings with
	 * underlines, generous spacing.
	 * `compact` — terminal replies: smaller headings, no underlines, tight margins.
	 */
	density?: 'comfortable' | 'compact';
	/**
	 * Stamp `data-source-line="N"` (the 1-based markdown source line) onto every
	 * rendered block element. Powers editor↔preview scroll sync in the markdown
	 * editor. Off by default — most consumers don't pay for it.
	 */
	sourceLines?: boolean;
}

/** Minimal hast shape — nodes carry `position` from remark-rehype, but
 *  react-markdown's re-exported types don't surface it. */
interface HastNode {
	type: string;
	properties?: Record<string, unknown>;
	position?: { start?: { line?: number } };
	children?: HastNode[];
}

// Walk the hast tree and stamp the source line onto every element that still
// carries position info (raw-HTML nodes reparsed by rehypeRaw may not — those
// are skipped, and the sync falls back to the nearest anchored block).
function rehypeSourceLines() {
	return (tree: HastNode) => {
		const walk = (node: HastNode) => {
			if (node.type === 'element' && node.position?.start?.line != null) {
				node.properties = node.properties ?? {};
				node.properties['data-source-line'] = node.position.start.line;
			}
			node.children?.forEach(walk);
		};
		walk(tree);
	};
}

export function Markdown({
	content,
	className,
	cwd,
	allowHtml = false,
	density = 'comfortable',
	sourceLines = false,
}: MarkdownProps) {
	const isCompact = density === 'compact';
	const components = useMemo(() => buildComponents(cwd), [cwd]);
	const autolinkOpts = {
		behavior: 'append' as const,
		properties: { className: 'heading-anchor', ariaHidden: 'true', tabIndex: -1 },
		content: { type: 'text', value: '' },
	};
	const rehypePlugins: PluggableList = [
		...(allowHtml ? [rehypeRaw] : []),
		rehypeSlug,
		[rehypeAutolinkHeadings, autolinkOpts],
		// Runs last so positions survive the earlier passes.
		...(sourceLines ? [rehypeSourceLines] : []),
	];
	return (
		<article
			className={cn(
				// Baseline: 16px / 1.5 by default (doc viewer). Compact callers override
				// via className with `text-sm` / `leading-relaxed` and pass density="compact".
				'text-[16px] leading-[1.5] text-foreground',
				// Heading scale: h1=2em, h2=1.5em, h3=1.25em, h4=1em (GitHub-style, em-based so it scales with body).
				'[&_h1]:text-[2em] [&_h1]:font-semibold [&_h1]:tracking-tight',
				'[&_h2]:text-[1.5em] [&_h2]:font-semibold [&_h2]:tracking-tight',
				'[&_h3]:text-[1.25em] [&_h3]:font-semibold',
				'[&_h4]:text-[1em] [&_h4]:font-semibold',
				// Heading margins + ornaments — comfortable (doc) vs compact.
				isCompact
					? [
							'[&_h1]:mt-4 [&_h1]:mb-2',
							'[&_h2]:mt-4 [&_h2]:mb-2',
							'[&_h3]:mt-3 [&_h3]:mb-1.5',
							'[&_h4]:mt-3 [&_h4]:mb-1.5',
						]
					: [
							'[&_h1]:mt-6 [&_h1]:mb-4 [&_h1]:pb-2 [&_h1]:border-b [&_h1]:border-border',
							'[&_h2]:mt-6 [&_h2]:mb-4 [&_h2]:pb-2 [&_h2]:border-b [&_h2]:border-border',
							'[&_h3]:mt-6 [&_h3]:mb-4',
							'[&_h4]:mt-6 [&_h4]:mb-4',
						],
				'[&_h1:first-child]:mt-0 [&_h2:first-child]:mt-0 [&_h3:first-child]:mt-0',
				// Hide autolink anchors injected by rehype-autolink-headings.
				'[&_a.heading-anchor]:hidden',
				// Paragraphs.
				isCompact ? '[&_p]:my-2' : '[&_p]:my-4',
				// Lists — compact density uses tighter indent.
				isCompact
					? '[&_ul]:my-2 [&_ol]:my-2 [&_ul]:pl-5 [&_ol]:pl-5 [&_ul]:list-disc [&_ol]:list-decimal'
					: '[&_ul]:my-4 [&_ol]:my-4 [&_ul]:pl-8 [&_ol]:pl-8 [&_ul]:list-disc [&_ol]:list-decimal',
				'[&_li]:my-1 [&_li>p]:my-1',
				'[&_li_ul]:my-1 [&_li_ol]:my-1',
				// Inline code.
				'[&_code]:rounded [&_code]:bg-muted [&_code]:px-[0.4em] [&_code]:py-[0.2em] [&_code]:text-[85%] [&_code]:font-mono',
				// Code fences (CodeFence already styles itself; keep <pre> neutral).
				'[&_pre]:my-3 [&_pre]:p-0 [&_pre]:bg-transparent',
				'[&_pre_code]:bg-transparent [&_pre_code]:p-0 [&_pre_code]:text-[100%]',
				// Links.
				'[&_a]:text-primary [&_a]:no-underline hover:[&_a]:underline',
				// Tables — visible cell borders, header bg.
				'[&_table]:my-3 [&_table]:w-fit [&_table]:max-w-full [&_table]:border-collapse [&_table]:text-[0.9em]',
				'[&_th]:border [&_th]:border-border [&_th]:bg-muted/50 [&_th]:px-3 [&_th]:py-1.5 [&_th]:text-left [&_th]:font-semibold',
				'[&_td]:border [&_td]:border-border [&_td]:px-3 [&_td]:py-1.5 [&_td]:align-top',
				// Horizontal rule.
				isCompact ? '[&_hr]:my-4' : '[&_hr]:my-8',
				'[&_hr]:border-0 [&_hr]:border-t [&_hr]:border-border',
				// Blockquote.
				'[&_blockquote]:my-3 [&_blockquote]:border-l-4 [&_blockquote]:border-border [&_blockquote]:pl-4 [&_blockquote]:text-muted-foreground',
				// Strong.
				'[&_strong]:font-semibold',
				// Images.
				'[&_img]:max-w-full [&_img]:rounded',
				className
			)}
		>
			<ReactMarkdown
				remarkPlugins={[remarkGfm]}
				rehypePlugins={rehypePlugins}
				components={components}
			>
				{content}
			</ReactMarkdown>
		</article>
	);
}

// ─── Components ─────────────────────────────────────────────────────────────

function buildComponents(cwd?: string) {
	return {
		code(
			props: {
				className?: string;
				children?: ReactNode;
			} & React.HTMLAttributes<HTMLElement>
		) {
			const { className, children } = props;
			// react-markdown v9+ stopped passing `inline`. Two signals must agree
			// before we render a block CodeFence: (1) `language-X` className,
			// (2) a newline in the body. Inline code never has newlines, so
			// requiring both prevents `<pre>` nesting inside `<p>` when remark
			// hands us a one-token inline that happens to pick up a language
			// class (streaming fences, ```text inline runs, etc.).
			const langMatch = /language-(\w+)/.exec(className ?? '');
			const text = String(children);
			const isBlock = !!langMatch && text.includes('\n');
			if (!isBlock) {
				if (looksLikePath(text)) {
					return <FilePathPill rawPath={text} cwd={cwd} display={text} />;
				}
				return (
					<code className="rounded bg-muted px-1.5 py-0.5 font-mono text-[0.9em]">{children}</code>
				);
			}
			return <CodeFence code={text.replace(/\n$/, '')} lang={langMatch![1]} />;
		},
		// Code fences arrive wrapped in a <pre> by react-markdown. Our CodeFence
		// emits its own <pre>, so we'd nest <pre><pre>. Render <pre>'s children
		// as a fragment to flatten that.
		pre(props: { children?: ReactNode }) {
			return <>{props.children}</>;
		},
		a(props: { href?: string; children?: ReactNode }) {
			const { href = '', children } = props;
			// Local file links → open in viewer pane.
			if (looksLikePath(href)) {
				return <FilePathPill rawPath={href} cwd={cwd} display={String(children)} />;
			}
			return (
				<a href={href} target="_blank" rel="noreferrer" className="text-primary hover:underline">
					{children}
				</a>
			);
		},
	} as const;
}

// ─── Code fence with copy button + shiki highlight ──────────────────────────

let shikiHighlighter: Promise<{
	highlight: (code: string, lang: string, theme: string) => Promise<string>;
}> | null = null;

function loadHighlighter() {
	if (!shikiHighlighter) {
		shikiHighlighter = import('shiki').then((mod) => ({
			highlight: (code, lang, theme) =>
				mod.codeToHtml(code, {
					lang: (lang as never) || 'text',
					theme: theme as never,
				}),
		}));
	}
	return shikiHighlighter;
}

function CodeFence({ code, lang }: { code: string; lang: string }) {
	const { resolvedTheme } = useTheme();
	const isDark = resolvedTheme === 'dark';
	const [html, setHtml] = useState<string | null>(null);
	const [copied, setCopied] = useState(false);

	useEffect(() => {
		let cancelled = false;
		loadHighlighter()
			.then((h) => h.highlight(code, lang, isDark ? 'github-dark' : 'github-light'))
			.then((out) => {
				if (!cancelled) setHtml(out);
			})
			.catch(() => {
				if (!cancelled) setHtml(null);
			});
		return () => {
			cancelled = true;
		};
	}, [code, lang, isDark]);

	async function handleCopy() {
		try {
			await navigator.clipboard.writeText(code);
			setCopied(true);
			setTimeout(() => setCopied(false), 1200);
		} catch (e) {
			console.warn('clipboard:', e);
		}
	}

	return (
		<div className="group relative my-3 overflow-hidden rounded-md border border-border bg-muted">
			<div className="flex items-center justify-between border-b border-border bg-muted/60 px-3 py-1.5 text-[10px] uppercase tracking-wide text-muted-foreground">
				<span className="font-mono">{lang === 'text' ? '' : lang}</span>
				<button
					type="button"
					onClick={handleCopy}
					className={cn(
						'inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] transition-colors',
						'opacity-0 group-hover:opacity-100',
						'hover:bg-muted-foreground/10',
						copied ? 'text-emerald-600 dark:text-emerald-400' : ''
					)}
					aria-label="Copy code"
				>
					{copied ? <Check className="h-3 w-3" /> : <Copy className="h-3 w-3" />}
					{copied ? 'copied' : 'copy'}
				</button>
			</div>
			{html ? (
				<div
					className="overflow-x-auto [&_pre]:!bg-transparent [&_pre]:p-3 [&_pre]:text-xs [&_pre]:m-0"
					dangerouslySetInnerHTML={{ __html: html }}
				/>
			) : (
				<pre className="overflow-x-auto p-3 text-xs">
					<code>{code}</code>
				</pre>
			)}
		</div>
	);
}

// ─── File path pill (inline) ────────────────────────────────────────────────

function FilePathPill({
	rawPath,
	cwd,
	display,
}: {
	rawPath: string;
	cwd: string | undefined;
	display: string;
}) {
	const focusedId = usePaneStore((s) => s.focusedId);
	const addTabBackground = usePaneStore((s) => s.addTabBackground);
	// Hold the live (post-fallback) resolution so the hover title and the click
	// target stay in sync. Initial state is the sync guess for first paint.
	const [resolvedPath, setResolvedPath] = useState(() => resolvePath(rawPath, cwd));

	useEffect(() => {
		let cancelled = false;
		resolvePathCached(rawPath, cwd).then((p) => {
			if (!cancelled) setResolvedPath(p);
		});
		return () => {
			cancelled = true;
		};
	}, [rawPath, cwd]);

	return (
		<button
			type="button"
			onClick={async () => {
				// Re-resolve on click in case the cache hasn't filled yet.
				const resolved = await resolvePathCached(rawPath, cwd);
				addTabBackground(focusedId, { kind: 'artifact', path: resolved });
			}}
			className="inline-flex items-baseline gap-1 rounded border border-violet-500/30 bg-violet-500/10 px-1 py-0 font-mono text-[0.85em] text-foreground transition-colors hover:border-violet-500/60 hover:bg-violet-500/20"
			title={`Open ${resolvedPath} in viewer`}
		>
			<FileText className="h-3 w-3 self-center text-violet-700 dark:text-violet-300" />
			{display}
		</button>
	);
}

// ─── Path resolution (markdown surface) ─────────────────────────────────────
// Detection (`looksLikePath`) and sync resolution (`resolvePath`) live in
// `@/lib/paths/file-paths` so the terminal link provider shares them. The
// async monorepo-disambiguation walk below is markdown-specific.

// Monorepo subproject names used as a disambiguation hint for `preferredSubproject`.
// Empty by default; the developer ergonomics use-case is to populate this from a
// user-configurable setting (future work). When empty, `preferredSubproject` is a
// no-op and path resolution falls back to the cwd/cache heuristics.
const MONOREPO_SUBPROJECTS: readonly string[] = [];

/** Snapshot the focused pane's active artifact view; if its path is rooted at
 *  the monorepo, return the immediate subproject. Used as a disambiguation
 *  hint so a path like `package.json` that exists in many subprojects prefers
 *  the one the user is currently looking at. */
function preferredSubproject(monorepoRoot: string): string | null {
	try {
		const state = usePaneStore.getState();
		const leaf = findLeaf(state.root, state.focusedId);
		const view = leaf?.tabs[leaf.activeTabIdx];
		if (view?.kind !== 'artifact') return null;
		const prefix = `${monorepoRoot}/`;
		if (!view.path.startsWith(prefix)) return null;
		const sub = view.path.slice(prefix.length).split('/')[0];
		return MONOREPO_SUBPROJECTS.includes(sub) ? sub : null;
	} catch {
		return null;
	}
}

// Memoize resolutions so a markdown doc with N pills referencing the same
// path doesn't fire N parallel IPC waves. Bounded to 500 entries to prevent
// unbounded memory growth over long sessions. Keyed by (cwd, rawPath).
const MAX_RESOLVE_CACHE_SIZE = 500;
const resolveCache = new Map<string, Promise<string>>();

function resolvePathCached(rawPath: string, cwd: string | undefined): Promise<string> {
	const key = `${cwd ?? ''}|${rawPath}`;
	let cached = resolveCache.get(key);
	if (!cached) {
		if (resolveCache.size >= MAX_RESOLVE_CACHE_SIZE) {
			const oldestKey = resolveCache.keys().next().value;
			if (oldestKey !== undefined) resolveCache.delete(oldestKey);
		}
		cached = resolvePathWithFallback(rawPath, cwd);
		resolveCache.set(key, cached);
	}
	return cached;
}

async function resolvePathWithFallback(p: string, cwd: string | undefined): Promise<string> {
	const home = await loadHome();
	const monorepoRoot = `${home}/royalti-co`;
	const initial = resolvePath(p, cwd);
	if (await fsExists(initial)) return initial;

	// Only relative paths get the subproject walk — absolute / ~ paths are
	// fully specified by the user and shouldn't be guessed at.
	const trimmed = p.trim();
	if (trimmed.startsWith('/') || trimmed.startsWith('~')) return initial;

	const cleaned = trimmed.replace(/^\.?\/+/, '');

	// Bump the focused pane's subproject to the front of the search order so
	// ambiguous filenames (e.g. `package.json`) prefer the project the user is
	// currently working in.
	const preferred = preferredSubproject(monorepoRoot);
	const ordered = preferred
		? [preferred, ...MONOREPO_SUBPROJECTS.filter((s) => s !== preferred)]
		: MONOREPO_SUBPROJECTS;

	// Issue all existence checks in parallel; pick the first match by priority
	// order. Worst case: one IPC roundtrip total instead of N sequential.
	const candidates = ordered.map((sub) => `${monorepoRoot}/${sub}/${cleaned}`);
	const results = await Promise.all(candidates.map((c) => fsExists(c)));
	const idx = results.findIndex(Boolean);
	return idx >= 0 ? candidates[idx] : initial;
}
