import { useCallback, useEffect, useRef, useState } from 'react';
import { AlertCircle, Info, Loader2 } from 'lucide-react';
import type { CodeEditorHandle } from '@ikenga/ui-lib';
import { Markdown } from '@/components/markdown';
import { fsRead } from '@/lib/tauri-cmd';
import { TextDocumentFrame } from '../editing/editable-text-frame';
import { useTextDocument } from '../editing/use-text-document';
import { MarkdownFormatControls } from './markdown-toolbar';
import { formatMarkdown, insertLink, toggleLinePrefix, wrapSelection } from './markdown-format';
import { useScrollSync } from './use-scroll-sync';

interface MarkdownViewProps {
	path: string;
	/** When true, show the Edit toggle + split source/preview editor. Defaults
	 *  to false so thumbnails and read-only embeds are unaffected. */
	editable?: boolean;
	/** The pane this view lives in — scopes its editing session. */
	paneId?: string;
	line?: number;
	col?: number;
}

const decode = (bytes: number[] | Uint8Array) =>
	new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(bytes));

export function MarkdownView({ path, editable = false, paneId, line, col }: MarkdownViewProps) {
	if (!editable) return <MarkdownReadOnly path={path} />;
	return <EditableMarkdown path={path} paneId={paneId} line={line} col={col} />;
}

/** Resolve relative links inside the doc against the file's directory. */
function cwdOf(path: string): string {
	return path.replace(/\/[^/]+$/, '');
}

// Read-only path — byte-for-byte the prior behavior (no toolbar). Keeps
// thumbnails and non-editable embeds untouched.
function MarkdownReadOnly({ path }: { path: string }) {
	const [state, setState] = useState<
		{ kind: 'loading' } | { kind: 'ready'; body: string } | { kind: 'error'; message: string }
	>({ kind: 'loading' });

	useEffect(() => {
		let cancelled = false;
		setState({ kind: 'loading' });
		fsRead(path)
			.then((res) => {
				if (!cancelled) setState({ kind: 'ready', body: decode(res.bytes) });
			})
			.catch((err) => {
				if (!cancelled)
					setState({ kind: 'error', message: err instanceof Error ? err.message : String(err) });
			});
		return () => {
			cancelled = true;
		};
	}, [path]);

	if (state.kind === 'loading') {
		return (
			<div className="flex h-full items-center justify-center text-xs text-muted-foreground">
				<Loader2 className="mr-2 h-4 w-4 animate-spin" /> Loading…
			</div>
		);
	}
	if (state.kind === 'error') {
		return (
			<div className="flex h-full items-start justify-center p-6 text-xs text-destructive">
				<AlertCircle className="mr-2 mt-0.5 h-4 w-4 shrink-0" />
				<span className="break-all">{state.message}</span>
			</div>
		);
	}
	return (
		<div className="h-full overflow-auto">
			<div className="mx-auto w-full max-w-[72ch] px-8 py-8">
				<Markdown content={state.body} cwd={cwdOf(path)} allowHtml />
			</div>
		</div>
	);
}

// Editable path — the shared editing surface (src/viewer/editing/) with the
// Markdown-specific parts: formatting controls, ⌘B/⌘I, and a live split
// preview whose scroll follows the editor.
function EditableMarkdown({
	path,
	paneId,
	line,
	col,
}: {
	path: string;
	paneId?: string;
	line?: number;
	col?: number;
}) {
	const doc = useTextDocument({ path, paneId });
	const [formatNote, setFormatNote] = useState<{ error: boolean; text: string } | null>(null);
	const editorRef = useRef<CodeEditorHandle>(null);
	const previewRef = useRef<HTMLDivElement>(null);
	const cwd = cwdOf(path);

	// ── Editor actions (operate on the live CodeMirror view) ─────────────────
	const withView = useCallback(
		(fn: (v: NonNullable<ReturnType<CodeEditorHandle['view']>>) => void) => {
			const v = editorRef.current?.view();
			if (v) fn(v);
		},
		[]
	);
	const onWrap = useCallback(
		(b: string, a?: string) => withView((v) => wrapSelection(v, b, a)),
		[withView]
	);
	const onPrefix = useCallback((p: string) => withView((v) => toggleLinePrefix(v, p)), [withView]);
	const onLink = useCallback(() => withView((v) => insertLink(v)), [withView]);
	// Format runs as a buffer operation (use-text-document "Ordering"): queued
	// behind a save or reload, and its result is applied only to the exact
	// text it formatted. A reload or typing meanwhile drops it — applying it
	// would put the old text back over the new.
	const { transform } = doc;
	const onFormatDoc = useCallback(async () => {
		setFormatNote(null);
		try {
			const result = await transform(formatMarkdown);
			if (result === 'stale') {
				setFormatNote({
					error: false,
					text: 'Format not applied: the text changed while it was being formatted. Run it again.',
				});
			}
		} catch (err) {
			setFormatNote({
				error: true,
				text: `Format failed: ${err instanceof Error ? err.message : String(err)}`,
			});
		}
	}, [transform]);

	const getView = useCallback(() => editorRef.current?.view() ?? null, []);
	const { onPreviewScroll } = useScrollSync({
		getView,
		previewRef,
		enabled: doc.mode === 'edit',
	});

	// WP-56 (G-ACTIONS §10.2/§10.6): `markdown.save/bold/italic` are registry
	// commands scoped by `markdownEditorFocus`, which the frame marks only in
	// Edit (DEC-59: `markdown.bold` outranks `explorer.toggle` on mod+b only
	// while that key holds). Bold/italic keep their extra CodeMirror-focus
	// check — the marker only says the editor pane has DOM focus, not that the
	// CodeMirror view specifically does.
	const commands = {
		'markdown.bold': () => {
			if (editorRef.current?.view()?.hasFocus) onWrap('**');
		},
		'markdown.italic': () => {
			if (editorRef.current?.view()?.hasFocus) onWrap('_');
		},
	};

	return (
		<TextDocumentFrame
			doc={doc}
			path={path}
			language="markdown"
			ariaLabel="Markdown source"
			focusArea="markdown-editor"
			commands={commands}
			editorRef={editorRef}
			line={line}
			col={col}
			toolbarExtras={
				<MarkdownFormatControls
					formatting={doc.busy === 'transform'}
					formatDisabled={doc.busy !== null || doc.blocked !== null}
					onFormatDoc={() => void onFormatDoc()}
					onWrap={onWrap}
					onPrefix={onPrefix}
					onLink={onLink}
				/>
			}
			banner={
				formatNote?.error ? (
					<div className="flex items-start gap-2 border-b border-destructive/40 bg-destructive/10 px-4 py-1.5 text-[11px] text-destructive">
						<AlertCircle className="mt-0.5 h-3 w-3 shrink-0" />
						<span className="break-all">{formatNote.text}</span>
					</div>
				) : formatNote && doc.mode === 'edit' ? (
					<div
						role="status"
						data-state="markdown-format-stale"
						className="flex items-center gap-2 border-b border-border bg-muted/30 px-4 py-1 text-[11px] text-muted-foreground"
					>
						<Info className="h-3 w-3 shrink-0" />
						<span>{formatNote.text}</span>
					</div>
				) : null
			}
			renderView={(text) => (
				<div className="h-full overflow-auto">
					<div className="mx-auto w-full max-w-[72ch] px-8 py-8">
						<Markdown content={text} cwd={cwd} allowHtml />
					</div>
				</div>
			)}
			renderPreview={(text) => (
				<div ref={previewRef} className="h-full overflow-auto" onScroll={onPreviewScroll}>
					<div className="mx-auto w-full max-w-[72ch] px-8 py-8">
						<Markdown content={text} cwd={cwd} allowHtml sourceLines />
					</div>
				</div>
			)}
		/>
	);
}
