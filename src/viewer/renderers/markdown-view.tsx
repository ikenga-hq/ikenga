import { type FocusEvent, useCallback, useEffect, useRef, useState } from 'react';
import { Panel, PanelGroup, PanelResizeHandle } from 'react-resizable-panels';
import { AlertCircle, Loader2, RefreshCw } from 'lucide-react';
import { CodeEditor, type CodeEditorHandle } from '@ikenga/ui-lib';
import { Markdown } from '@/components/markdown';
import { focusMarkerProps } from '@/lib/keymap/context-keys';
import { useCommands } from '@/lib/keymap/dispatcher';
import { fsListenWatch, fsRead, fsUnwatch, fsWatch, fsWriteText } from '@/lib/tauri-cmd';
import type { UnlistenFn } from '@/lib/transport';
import { MarkdownToolbar, type SaveState } from './markdown-toolbar';
import { formatMarkdown, insertLink, toggleLinePrefix, wrapSelection } from './markdown-format';
import { useScrollSync } from './use-scroll-sync';

interface MarkdownViewProps {
	path: string;
	/** When true, show the Edit toggle + split source/preview editor. Defaults
	 *  to false so thumbnails and read-only embeds are unaffected. */
	editable?: boolean;
	line?: number;
	col?: number;
}

type LoadState =
	| { kind: 'loading' }
	| { kind: 'ready'; body: string }
	| { kind: 'error'; message: string };

const decode = (bytes: number[] | Uint8Array) =>
	new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(bytes));

export function MarkdownView({ path, editable = false, line, col }: MarkdownViewProps) {
	const [state, setState] = useState<LoadState>({ kind: 'loading' });
	// `body` is the last-known on-disk content; `draft` is the editor buffer.
	// They diverge while editing and re-converge on a successful save.
	const [draft, setDraft] = useState('');
	const [mode, setMode] = useState<'preview' | 'edit'>('preview');
	const [saveState, setSaveState] = useState<SaveState>({ kind: 'idle' });
	const [formatting, setFormatting] = useState(false);
	// Set when the file changes on disk under us (external editor, git, agent).
	const [external, setExternal] = useState<string | null>(null);

	const editorRef = useRef<CodeEditorHandle>(null);
	const previewRef = useRef<HTMLDivElement>(null);

	const body = state.kind === 'ready' ? state.body : '';
	const dirty = state.kind === 'ready' && draft !== body;

	// Refs the disk watcher reads without re-subscribing on every keystroke.
	const bodyRef = useRef(body);
	const lastSavedRef = useRef<string | null>(null);
	const ackedDiskRef = useRef<string | null>(null);
	useEffect(() => {
		bodyRef.current = body;
	}, [body]);

	useEffect(() => {
		let cancelled = false;
		setState({ kind: 'loading' });
		setMode('preview');
		setSaveState({ kind: 'idle' });
		setExternal(null);
		ackedDiskRef.current = null;
		lastSavedRef.current = null;
		fsRead(path)
			.then((res) => {
				if (cancelled) return;
				const text = decode(res.bytes);
				setState({ kind: 'ready', body: text });
				setDraft(text);
			})
			.catch((err) => {
				if (cancelled) return;
				setState({ kind: 'error', message: err instanceof Error ? err.message : String(err) });
			});
		return () => {
			cancelled = true;
		};
	}, [path]);

	const save = useCallback(async () => {
		if (state.kind !== 'ready' || draft === bodyRef.current) return;
		const next = draft;
		setSaveState({ kind: 'saving' });
		try {
			await fsWriteText(path, next);
			lastSavedRef.current = next; // so the watcher ignores our own write
			setState({ kind: 'ready', body: next });
			setSaveState({ kind: 'idle' });
		} catch (err) {
			setSaveState({ kind: 'error', message: err instanceof Error ? err.message : String(err) });
		}
	}, [state.kind, draft, path]);

	// Cursor jump & scroll for CodeMirror 6 editor (WP-05 / T-04)
	useEffect(() => {
		if (!line) return;
		const v = editorRef.current?.view();
		if (!v) return;
		try {
			const docLines = v.state.doc.lines;
			const targetLine = Math.min(Math.max(1, line), docLines);
			const docLine = v.state.doc.line(targetLine);
			const targetCol = Math.max(1, col ?? 1);
			const pos = Math.min(docLine.from + targetCol - 1, docLine.to);
			v.dispatch({
				selection: { anchor: pos },
				scrollIntoView: true,
			});
		} catch {}
	}, [line, col, mode, state.kind]);

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
	const onFormatDoc = useCallback(async () => {
		setFormatting(true);
		try {
			setDraft(await formatMarkdown(draft));
		} catch (err) {
			setSaveState({
				kind: 'error',
				message: `Format failed: ${err instanceof Error ? err.message : String(err)}`,
			});
		} finally {
			setFormatting(false);
		}
	}, [draft]);

	// ── Keyboard: ⌘S save · ⌘B/⌘I bold/italic while editing ──────────────────
	// WP-56 (G-ACTIONS §10.2/§10.6): migrated from a window-level `keydown`
	// listener to registry commands (`markdown.save/bold/italic`), scoped by
	// the new `markdownEditorFocus` key (B-21, marked on the edit container
	// below) instead of the `mode === 'edit'` closure guard. Bold/italic keep
	// their extra CodeMirror-focus check — the marker only says the editor
	// pane has DOM focus, not that the CodeMirror view specifically does.
	// `markdown.bold` shares `mod+b` with `explorer.toggle` (`always`) by
	// precedence, not a clash (DEC-59, §2.3): `markdownEditorFocus` is more
	// specific and wins while it holds.
	//
	// Fix round 1: `commands.ts` keys a handler stack by command id only —
	// with two markdown editors open in a split, the last-mounted one always
	// wins ⌘S/⌘B/⌘I regardless of which pane the user is actually in. Track
	// focus-within on this root and register only while it holds (and only
	// in edit mode, as before).
	const [focusWithin, setFocusWithin] = useState(false);
	const handleFocusWithinCapture = useCallback(() => setFocusWithin(true), []);
	const handleBlurWithinCapture = useCallback((e: FocusEvent<HTMLDivElement>) => {
		if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setFocusWithin(false);
	}, []);
	useCommands(
		{
			'markdown.save': () => void save(),
			'markdown.bold': () => {
				if (editorRef.current?.view()?.hasFocus) onWrap('**');
			},
			'markdown.italic': () => {
				if (editorRef.current?.view()?.hasFocus) onWrap('_');
			},
		},
		{ enabled: mode === 'edit' && focusWithin }
	);

	// ── Warn before losing unsaved edits on app close / reload ───────────────
	useEffect(() => {
		if (!dirty) return;
		const onBeforeUnload = (e: BeforeUnloadEvent) => {
			e.preventDefault();
			e.returnValue = '';
		};
		window.addEventListener('beforeunload', onBeforeUnload);
		return () => window.removeEventListener('beforeunload', onBeforeUnload);
	}, [dirty]);

	// ── Detect external on-disk changes ──────────────────────────────────────
	useEffect(() => {
		if (!editable) return;
		let active = true;
		let unlisten: UnlistenFn | undefined;
		let watcherId: string | undefined;
		(async () => {
			try {
				watcherId = await fsWatch(path);
				unlisten = await fsListenWatch(watcherId, async () => {
					try {
						const disk = decode((await fsRead(path)).bytes);
						if (!active) return;
						// Ignore our own writes, no-op events, and content already acked.
						if (disk === lastSavedRef.current) return;
						if (disk === bodyRef.current) return;
						if (disk === ackedDiskRef.current) return;
						setExternal(disk);
					} catch {
						/* file may be mid-write or deleted; ignore transient errors */
					}
				});
			} catch {
				/* watching is best-effort */
			}
		})();
		return () => {
			active = false;
			unlisten?.();
			if (watcherId) void fsUnwatch(watcherId);
		};
	}, [editable, path]);

	const reloadFromDisk = useCallback(() => {
		if (external == null) return;
		setState({ kind: 'ready', body: external });
		setDraft(external);
		setExternal(null);
	}, [external]);

	const dismissExternal = useCallback(() => {
		ackedDiskRef.current = external; // stop re-nagging for this same content
		setExternal(null);
	}, [external]);

	const getView = useCallback(() => editorRef.current?.view() ?? null, []);
	const { onPreviewScroll } = useScrollSync({
		getView,
		previewRef,
		enabled: mode === 'edit',
	});

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

	// Resolve relative links inside the doc against the file's directory.
	const cwd = path.replace(/\/[^/]+$/, '');

	// Read-only path — byte-for-byte the prior behavior (no toolbar). Keeps
	// thumbnails and non-editable embeds untouched.
	if (!editable) {
		return (
			<div className="h-full overflow-auto">
				<div className="mx-auto w-full max-w-[72ch] px-8 py-8">
					<Markdown content={body} cwd={cwd} allowHtml />
				</div>
			</div>
		);
	}

	return (
		<div
			className="flex h-full w-full flex-col"
			// Fix round 1: only in edit mode — this wrapper renders in preview
			// mode too, and `markdown.bold`'s `when: markdownEditorFocus`
			// outranking `explorer.toggle` on `mod+b` there (DEC-59 specificity)
			// but the handler being `enabled: mode === 'edit'`-gated meant ⌘B did
			// nothing at all in preview instead of falling through to Explorer.
			{...(mode === 'edit' ? focusMarkerProps('markdown-editor') : {})}
			onFocusCapture={handleFocusWithinCapture}
			onBlurCapture={handleBlurWithinCapture}
		>
			<MarkdownToolbar
				mode={mode}
				dirty={dirty}
				saveState={saveState}
				formatting={formatting}
				onToggle={() => setMode((m) => (m === 'edit' ? 'preview' : 'edit'))}
				onSave={() => void save()}
				onFormatDoc={() => void onFormatDoc()}
				onWrap={onWrap}
				onPrefix={onPrefix}
				onLink={onLink}
			/>
			{external != null && (
				<div className="flex items-center gap-2 border-b border-amber-500/40 bg-amber-500/10 px-4 py-1.5 text-[11px] text-amber-700 dark:text-amber-300">
					<RefreshCw className="h-3 w-3 shrink-0" />
					<span>This file changed on disk.</span>
					<button
						type="button"
						onClick={reloadFromDisk}
						className="rounded px-1.5 py-0.5 font-medium underline-offset-2 hover:underline"
					>
						{dirty ? 'Reload (discard your changes)' : 'Reload'}
					</button>
					<button
						type="button"
						onClick={dismissExternal}
						className="rounded px-1.5 py-0.5 font-medium hover:underline"
					>
						Keep editing
					</button>
				</div>
			)}
			{saveState.kind === 'error' && (
				<div className="flex items-start gap-2 border-b border-destructive/40 bg-destructive/10 px-4 py-1.5 text-[11px] text-destructive">
					<AlertCircle className="mt-0.5 h-3 w-3 shrink-0" />
					<span className="break-all">{saveState.message}</span>
				</div>
			)}
			<div className="min-h-0 flex-1">
				{mode === 'preview' ? (
					<div className="h-full overflow-auto">
						<div className="mx-auto w-full max-w-[72ch] px-8 py-8">
							<Markdown content={body} cwd={cwd} allowHtml />
						</div>
					</div>
				) : (
					<PanelGroup direction="horizontal" className="h-full w-full">
						<Panel defaultSize={50} minSize={25} className="min-w-0">
							<CodeEditor
								ref={editorRef}
								value={draft}
								onChange={setDraft}
								language="markdown"
								ariaLabel="Markdown source"
							/>
						</Panel>
						<PanelResizeHandle className="w-px bg-border transition-colors hover:bg-primary/40 data-[resize-handle-active]:bg-primary/60" />
						<Panel defaultSize={50} minSize={25} className="min-w-0">
							<div ref={previewRef} className="h-full overflow-auto" onScroll={onPreviewScroll}>
								<div className="mx-auto w-full max-w-[72ch] px-8 py-8">
									<Markdown content={draft} cwd={cwd} allowHtml sourceLines />
								</div>
							</div>
						</Panel>
					</PanelGroup>
				)}
			</div>
		</div>
	);
}
