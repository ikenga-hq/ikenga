// The shared editing surface (plans/file-editing Shape 1): one toolbar, one
// set of banners, one CodeMirror editor (from @ikenga/ui-lib), hosted by every
// editable text renderer — Code, JSON, CSV, HTML source and Markdown.
//
// `EditableTextFrame` owns the document (one fsRead, handed to `renderView`, so
// a renderer no longer reads the file twice). `TextDocumentFrame` is the same
// surface for a renderer that holds the document itself (Markdown, whose split
// preview needs the editor ref and the mode for scroll sync).

import {
	type FocusEvent,
	type ReactNode,
	type RefObject,
	useCallback,
	useEffect,
	useMemo,
	useRef,
	useState,
} from 'react';
import { Panel, PanelGroup, PanelResizeHandle } from 'react-resizable-panels';
import { AlertCircle, Lock } from 'lucide-react';
import { CodeEditor, type CodeEditorHandle } from '@ikenga/ui-lib';
import { ErrorState, LoadingState } from '@/components/states';
import { focusMarkerProps } from '@/lib/keymap/context-keys';
import { useCommands } from '@/lib/keymap/dispatcher';
import { ConflictBanner } from './conflict-banner';
import { EditorToolbar } from './editor-toolbar';
import { type EditorLanguage, editorLanguageFor } from './language';
import { type TextDocument, useTextDocument } from './use-text-document';
import { type Validator, validationKindFor, validatorFor } from './validate';

type EditorFocusArea = 'text-editor' | 'markdown-editor';

export interface TextDocumentFrameProps {
	doc: TextDocument;
	path: string;
	language?: EditorLanguage;
	/** View-mode body, given the file's text. */
	renderView: (text: string) => ReactNode;
	/** When set, Edit is a split: editor left, this live preview right. */
	renderPreview?: (draft: string) => ReactNode;
	/** Format-specific toolbar controls, shown in Edit. */
	toolbarExtras?: ReactNode;
	/** A renderer-specific banner under the toolbar (e.g. Markdown's
	 *  "Format failed"). */
	banner?: ReactNode;
	/** Focus area marked while editing — decides which ⌘S command fires:
	 *  `editor.save` (default) or Markdown's own `markdown.save`. */
	focusArea?: EditorFocusArea;
	/** Extra keymap commands, live while editing with focus inside. */
	commands?: Record<string, () => void>;
	editorRef?: RefObject<CodeEditorHandle | null>;
	line?: number;
	col?: number;
	ariaLabel?: string;
}

export function TextDocumentFrame({
	doc,
	path,
	language,
	renderView,
	renderPreview,
	toolbarExtras,
	banner,
	focusArea = 'text-editor',
	commands,
	editorRef: externalRef,
	line,
	col,
	ariaLabel,
}: TextDocumentFrameProps) {
	const ownRef = useRef<CodeEditorHandle | null>(null);
	const editorRef = externalRef ?? ownRef;
	const editing = doc.mode === 'edit';
	const lang = language ?? editorLanguageFor(path);

	// ⌘S and friends. Register only while this editor holds focus, so with two
	// editors open in a split the one the user is in gets the key (the command
	// stack is keyed by id only — see markdown-view's history).
	const [focusWithin, setFocusWithin] = useState(false);
	const onFocusCapture = useCallback(() => setFocusWithin(true), []);
	const onBlurCapture = useCallback((e: FocusEvent<HTMLDivElement>) => {
		if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setFocusWithin(false);
	}, []);
	const saveCommand = focusArea === 'markdown-editor' ? 'markdown.save' : 'editor.save';
	useCommands(
		{ [saveCommand]: () => void doc.save(), ...commands },
		{ enabled: editing && focusWithin }
	);

	// Cursor jump for line/col links (WP-05 / T-04), and "Go to line" from a
	// validation error.
	const jumpTo = useCallback(
		(toLine: number, toCol?: number) => {
			const v = editorRef.current?.view();
			if (!v) return;
			try {
				const docLine = v.state.doc.line(Math.min(Math.max(1, toLine), v.state.doc.lines));
				const pos = Math.min(docLine.from + Math.max(1, toCol ?? 1) - 1, docLine.to);
				v.dispatch({ selection: { anchor: pos }, scrollIntoView: true });
				v.focus();
			} catch {}
		},
		[editorRef]
	);
	useEffect(() => {
		if (!line || !editing || doc.load.kind !== 'ready') return;
		jumpTo(line, col);
	}, [line, col, editing, doc.load.kind, jumpTo]);

	if (doc.load.kind === 'loading') {
		return <LoadingState data-state="loading" fill heading="Loading…" />;
	}
	if (doc.load.kind === 'error') {
		return (
			<ErrorState
				data-state="error"
				fill
				icon={AlertCircle}
				heading="Couldn't read this file"
				body={<span className="break-all">{doc.load.message}</span>}
			/>
		);
	}

	const editor = (
		<CodeEditor
			ref={editorRef}
			value={doc.draft}
			onChange={doc.setDraft}
			// Locked while Cancel or Load theirs reads the text it is about to
			// put here: anything typed meanwhile would be overwritten.
			readOnly={doc.blocked !== null || doc.busy === 'cancel' || doc.busy === 'load-theirs'}
			language={lang}
			ariaLabel={ariaLabel ?? 'File source'}
		/>
	);

	return (
		<div
			className="flex h-full w-full flex-col"
			data-state={editing ? 'text-document-edit' : 'text-document-view'}
			{...(editing ? focusMarkerProps(focusArea) : {})}
			onFocusCapture={onFocusCapture}
			onBlurCapture={onBlurCapture}
		>
			<EditorToolbar
				mode={doc.mode}
				dirty={doc.dirty}
				saveState={doc.saveState}
				busy={doc.busy}
				blocked={doc.blocked}
				unvalidated={validationKindFor(path) === 'unvalidated'}
				onEdit={doc.startEdit}
				onDone={doc.finishEdit}
				onCancel={() => void doc.cancel()}
				onSave={() => void doc.save()}
				extras={toolbarExtras}
			/>
			{editing && doc.conflict && (
				<ConflictBanner
					conflict={doc.conflict}
					mine={doc.draft}
					busy={doc.busy !== null}
					onKeepMine={() => void doc.keepMine()}
					onLoadTheirs={() => void doc.loadTheirs()}
					onDiscard={() => void doc.cancel()}
				/>
			)}
			{editing && doc.blocked && (
				<div
					role="alert"
					data-state="editor-blocked-edit"
					className="flex items-start gap-2 border-b border-amber-500/40 bg-amber-500/10 px-4 py-1.5 text-[11px] text-amber-700 dark:text-amber-300"
				>
					<Lock className="mt-0.5 h-3 w-3 shrink-0" />
					<span className="min-w-0 break-words">{doc.blocked}</span>
				</div>
			)}
			{editing && doc.validation && (
				<div
					role="alert"
					data-state="editor-invalid"
					className="flex items-start gap-2 border-b border-destructive/40 bg-destructive/10 px-4 py-1.5 text-[11px] text-destructive"
				>
					<AlertCircle className="mt-0.5 h-3 w-3 shrink-0" />
					<span className="min-w-0 break-words">{doc.validation.message} Not saved.</span>
					{doc.validation.line !== undefined && (
						<button
							type="button"
							onClick={() => jumpTo(doc.validation?.line ?? 1, doc.validation?.col)}
							className="shrink-0 rounded px-1.5 font-medium underline-offset-2 hover:underline"
						>
							Go to line {doc.validation.line}
						</button>
					)}
				</div>
			)}
			{doc.saveState.kind === 'error' && (
				<div
					role="alert"
					data-state="editor-save-error"
					className="flex items-start gap-2 border-b border-destructive/40 bg-destructive/10 px-4 py-1.5 text-[11px] text-destructive"
				>
					<AlertCircle className="mt-0.5 h-3 w-3 shrink-0" />
					<span className="break-all">Save failed: {doc.saveState.message}</span>
				</div>
			)}
			{banner}
			<div className="min-h-0 flex-1">
				{!editing ? (
					renderView(doc.viewText)
				) : renderPreview ? (
					<PanelGroup direction="horizontal" className="h-full w-full">
						<Panel defaultSize={50} minSize={25} className="min-w-0">
							{editor}
						</Panel>
						<PanelResizeHandle className="w-px bg-border transition-colors hover:bg-primary/40 data-[resize-handle-active]:bg-primary/60" />
						<Panel defaultSize={50} minSize={25} className="min-w-0">
							{renderPreview(doc.draft)}
						</Panel>
					</PanelGroup>
				) : (
					editor
				)}
			</div>
		</div>
	);
}

export interface EditableTextFrameProps extends Omit<TextDocumentFrameProps, 'doc'> {
	paneId?: string | null;
	/** Override the per-extension validator (null = no check). */
	validate?: Validator | null;
}

/** The editing surface plus its document — the common case. */
export function EditableTextFrame({ paneId, validate, ...rest }: EditableTextFrameProps) {
	const validator = useMemo(
		() => (validate === undefined ? validatorFor(rest.path) : validate),
		[validate, rest.path]
	);
	const doc = useTextDocument({ path: rest.path, paneId, validate: validator });
	return <TextDocumentFrame doc={doc} {...rest} />;
}
