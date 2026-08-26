import { lazy, Suspense, useEffect, useMemo, useState } from 'react';
import { FileText, Loader2 } from 'lucide-react';
import { basename } from './lib/path';
import { mimeFromExt, resolveMime } from './lib/mime';
import { recordViewerOpen, type ViewerRecentSource } from './lib/recents';
import { HtmlFrame } from './renderers/html-frame';
import { ImageView } from './renderers/image-view';
import { MarkdownView } from './renderers/markdown-view';
import { VideoView } from './renderers/video-view';
import { AudioView } from './renderers/audio-view';
import { PenView } from './renderers/pen-view';
import { UnknownView } from './renderers/unknown-view';

// Heavy renderers — code-split out of the main bundle. Each pulls a
// chunky dep (shiki ~300KB, react-pdf ~500KB, xlsx ~400KB) that most
// sessions never touch. Default-export shims live alongside each
// renderer so React.lazy can pick them up without touching the named
// export consumers elsewhere.
const CodeView = lazy(() => import('./renderers/code-view').then((m) => ({ default: m.CodeView })));
const PdfView = lazy(() => import('./renderers/pdf-view').then((m) => ({ default: m.PdfView })));
const XlsxView = lazy(() => import('./renderers/xlsx-view').then((m) => ({ default: m.XlsxView })));

interface ViewerRouterProps {
	path: string;
	/** Source for the recents log. Defaults to 'pane' since the side pane is
	 *  the primary surface today. */
	source?: ViewerRecentSource;
	/** Hide the filename header bar (when the host wants to render its own). */
	chromeless?: boolean;
	/** Forwarded to renderers that opt into iyke iframe bridging (HtmlFrame). */
	paneId?: string;
	/** Enable in-place editing for renderers that support it (MarkdownView).
	 *  Defaults false so thumbnails/embeds stay read-only. */
	editable?: boolean;
	line?: number;
	col?: number;
}

type Renderer = React.ComponentType<{ path: string }>;

function pickRenderer(mime: string, path: string): Renderer {
	const lower = path.toLowerCase();
	if (mime === 'text/html' || lower.endsWith('.html') || lower.endsWith('.htm')) return HtmlFrame;
	if (mime === 'application/pdf' || lower.endsWith('.pdf')) return PdfView;
	// .svg is image/* but renders best as a regular image; HTML frame would also
	// work but the image viewer's zoom/pan is friendlier.
	if (mime.startsWith('image/')) return ImageView;
	if (mime.startsWith('video/')) return VideoView;
	if (mime.startsWith('audio/')) return AudioView;
	if (lower.endsWith('.xlsx') || lower.endsWith('.xls')) return XlsxView;
	if (lower.endsWith('.pen')) return PenView;
	if (mime === 'text/markdown' || lower.endsWith('.md') || lower.endsWith('.mdx'))
		return MarkdownView;
	if (
		mime.startsWith('text/') ||
		mime === 'application/json' ||
		mime === 'application/yaml' ||
		mime === 'application/toml' ||
		mime === 'application/xml'
	)
		return CodeView;
	return UnknownView;
}

/** Auto-router that dispatches to the right renderer for `path`. Hybrid MIME
 * detection (sync extension lookup → fs_mime fallback) keeps the first paint
 * cheap for known types. */
export function ViewerRouter({
	path,
	source = 'pane',
	chromeless,
	paneId,
	editable,
	line,
	col,
}: ViewerRouterProps) {
	// Synchronous first-pass — known extensions render immediately.
	const initialMime = useMemo(() => mimeFromExt(path), [path]);
	const [mime, setMime] = useState<string | null>(initialMime ?? null);

	useEffect(() => {
		const fromExt = mimeFromExt(path);
		if (fromExt) {
			setMime(fromExt);
			return;
		}
		// Unknown extension — ask Rust.
		let cancelled = false;
		setMime(null);
		resolveMime(path).then((m) => {
			if (!cancelled) setMime(m);
		});
		return () => {
			cancelled = true;
		};
	}, [path]);

	// Record into viewer_recents on every (path, mime) pair we settle on. Best
	// effort — the table just powers a UX list.
	useEffect(() => {
		if (mime === null) return;
		void recordViewerOpen(path, mime, source);
	}, [path, mime, source]);

	if (mime === null) {
		return (
			<div className="flex h-full items-center justify-center text-xs text-muted-foreground">
				<Loader2 className="mr-2 h-4 w-4 animate-spin" /> Detecting type…
			</div>
		);
	}

	const Renderer = pickRenderer(mime, path);

	// Suspense fence for the lazy-loaded heavyweights (code/pdf/xlsx).
	// Eager renderers wrap through it cleanly — Suspense is a no-op when
	// nothing inside it suspends. HtmlFrame gets paneId so it can register
	// with iyke; other renderers don't need it.
	const body = (
		<Suspense fallback={<RendererLoading />}>
			{Renderer === HtmlFrame ? (
				<HtmlFrame path={path} paneId={paneId} />
			) : Renderer === MarkdownView ? (
				<MarkdownView path={path} editable={editable} />
			) : Renderer === CodeView ? (
				<CodeView path={path} line={line} col={col} />
			) : (
				<Renderer path={path} />
			)}
		</Suspense>
	);

	if (chromeless) {
		return body;
	}

	return (
		<div className="flex h-full w-full flex-col">
			<div className="flex shrink-0 items-center gap-2 border-b border-border bg-muted/20 px-4 py-2 text-xs">
				<FileText className="h-3.5 w-3.5 text-muted-foreground" />
				<span className="truncate font-medium text-foreground" title={path}>
					{basename(path)}
				</span>
				<span className="truncate text-muted-foreground" title={path}>
					— {path}
				</span>
				<span className="ml-auto font-mono text-[10px] text-muted-foreground">{mime}</span>
			</div>
			<div className="min-h-0 flex-1">{body}</div>
		</div>
	);
}

function RendererLoading() {
	return (
		<div className="flex h-full items-center justify-center text-xs text-muted-foreground">
			<Loader2 className="mr-2 h-4 w-4 animate-spin" /> Loading viewer…
		</div>
	);
}
