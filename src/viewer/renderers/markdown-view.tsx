import { useCallback, useEffect, useRef, useState } from 'react';
import { Panel, PanelGroup, PanelResizeHandle } from 'react-resizable-panels';
import { AlertCircle, Loader2, RefreshCw } from 'lucide-react';
import { CodeEditor, type CodeEditorHandle } from '@ikenga/ui-lib';
import { Markdown } from '@/components/markdown';
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
}

type LoadState =
	| { kind: 'loading' }
	| { kind: 'ready'; body: string }
	| { kind: 'error'; message: string };

const decode = (bytes: number[] | Uint8Array) =>
	new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(bytes));

export function MarkdownView({ path, editable = false }: MarkdownViewProps) {
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

	// ── Keyboard: ⌘/Ctrl + S / B / I while editing ───────────────────────────
	const saveRef = useRef(save);
	saveRef.current = save;
	useEffect(() => {
		if (mode !== 'edit') return;
		const onKeyDown = (e: KeyboardEvent) => {
			if (!(e.metaKey || e.ctrlKey)) return;
			const k = e.key.toLowerCase();
			if (k === 's') {
				e.preventDefault();
				void saveRef.current();
			} else if (k === 'b' && editorRef.current?.view()?.hasFocus) {
				e.preventDefault();
				onWrap('**');
			} else if (k === 'i' && editorRef.current?.view()?.hasFocus) {
				e.preventDefault();
				onWrap('_');
			}
		};
		window.addEventListener('keydown', onKeyDown);
		return () => window.removeEventListener('keydown', onKeyDown);
	}, [mode, onWrap]);

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
		<div className="flex h-full w-full flex-col">
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
