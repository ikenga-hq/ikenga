// The editing state machine behind every editable text renderer
// (plans/file-editing Shape 1 + 2), lifted out of markdown-view.tsx so Code,
// JSON, CSV, HTML source and Markdown share one implementation.
//
// - Read-only until `startEdit` (F4).
// - `base` is the disk text the editor is based on; `draft` is the buffer.
//   Both are LF text — see text-document.ts for how line endings and a BOM
//   survive the round trip.
// - `save` is conditional (F3): validate → re-read → compare with `base` →
//   write only if the file is unchanged. A changed file raises the conflict
//   choice instead of writing; a missing file raises the "deleted" choice.
// - The file watcher reloads a clean buffer quietly and raises the conflict
//   choice early for a dirty one. Our own write is recognised and ignored.
// - Unsaved edits survive an unmount (tab switch) through the editing store.

import { useCallback, useEffect, useRef, useState } from 'react';
import { sessionKey, useEditingStore } from '@/lib/editing/editing-store';
import { fsListenWatch, fsRead, fsUnwatch, fsWatch, fsWriteText } from '@/lib/tauri-cmd';
import type { UnlistenFn } from '@/lib/transport';
import {
	type DocumentMeta,
	decodeForEdit,
	decodeForView,
	encodeForSave,
	isConflict,
	normaliseText,
} from './text-document';
import type { ValidationResult, Validator } from './validate';

export type SaveState = { kind: 'idle' } | { kind: 'saving' } | { kind: 'error'; message: string };

export type Conflict = { kind: 'changed'; theirs: string } | { kind: 'deleted' };

export type LoadState =
	| { kind: 'loading' }
	| { kind: 'ready' }
	| { kind: 'error'; message: string };

export type ValidationError = Extract<ValidationResult, { ok: false }>;

export interface UseTextDocumentOptions {
	path: string;
	paneId?: string | null;
	validate?: Validator | null;
}

export interface TextDocument {
	load: LoadState;
	/** What View mode renders: the disk text (lossy-decoded if it cannot be
	 *  edited). */
	viewText: string;
	base: string;
	draft: string;
	setDraft: (next: string) => void;
	dirty: boolean;
	mode: 'view' | 'edit';
	/** Why Edit is unavailable (too large, binary, not UTF-8), or null. */
	blocked: string | null;
	saveState: SaveState;
	conflict: Conflict | null;
	validation: ValidationError | null;
	/** A short-lived note, e.g. "Reloaded — the file changed on disk." */
	notice: string | null;
	startEdit: () => void;
	/** Leave Edit. Only valid with no unsaved changes. */
	finishEdit: () => void;
	/** Discard the draft (draft = base) and return to View. */
	cancel: () => void;
	save: (opts?: { force?: boolean }) => Promise<void>;
	/** Conflict: overwrite the file with the draft. */
	keepMine: () => Promise<void>;
	/** Conflict: replace the draft with the file on disk. */
	loadTheirs: () => void;
	dismissValidation: () => void;
}

const NOTICE_MS = 4_000;

interface DocState {
	load: LoadState;
	viewText: string;
	base: string;
	meta: DocumentMeta;
	blocked: string | null;
}

const INITIAL: DocState = {
	load: { kind: 'loading' },
	viewText: '',
	base: '',
	meta: { eol: '\n', bom: false },
	blocked: null,
};

export function useTextDocument({
	path,
	paneId = null,
	validate,
}: UseTextDocumentOptions): TextDocument {
	const [doc, setDoc] = useState<DocState>(INITIAL);
	const [draft, setDraftState] = useState('');
	const [mode, setMode] = useState<'view' | 'edit'>('view');
	const [saveState, setSaveState] = useState<SaveState>({ kind: 'idle' });
	const [conflict, setConflict] = useState<Conflict | null>(null);
	const [validation, setValidation] = useState<ValidationError | null>(null);
	const [notice, setNotice] = useState<string | null>(null);

	// Refs the async paths read, so they see the latest values without
	// re-subscribing the watcher on every keystroke.
	const baseRef = useRef('');
	const draftRef = useRef('');
	const metaRef = useRef<DocumentMeta>(INITIAL.meta);
	const lastSavedRef = useRef<string | null>(null);
	const modeRef = useRef<'view' | 'edit'>('view');
	const savingRef = useRef(false);
	const noticeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

	const dirty = doc.load.kind === 'ready' && draft !== doc.base;

	const setDraft = useCallback((next: string) => {
		draftRef.current = next;
		setDraftState(next);
	}, []);

	const adoptBase = useCallback((text: string) => {
		baseRef.current = text;
		setDoc((d) => ({ ...d, base: text, viewText: text }));
	}, []);

	const flashNotice = useCallback((text: string) => {
		setNotice(text);
		if (noticeTimer.current) clearTimeout(noticeTimer.current);
		noticeTimer.current = setTimeout(() => setNotice(null), NOTICE_MS);
	}, []);

	useEffect(() => {
		modeRef.current = mode;
	}, [mode]);

	// ── Session registration (editing store) ────────────────────────────────
	const key = sessionKey(path, paneId);
	const upsert = useEditingStore((s) => s.upsert);
	useEffect(() => {
		upsert(key, { path, paneId, mounted: true, editing: mode === 'edit', dirty });
	}, [upsert, key, path, paneId, mode, dirty]);
	useEffect(() => {
		return () => {
			const store = useEditingStore.getState();
			const cur = store.sessions[key];
			const unsaved = draftRef.current !== baseRef.current && modeRef.current === 'edit';
			if (cur?.discarded || !unsaved) {
				store.remove(key);
				return;
			}
			store.upsert(key, {
				path,
				paneId,
				mounted: false,
				editing: false,
				dirty: true,
				stash: { draft: draftRef.current, base: baseRef.current, meta: metaRef.current },
			});
		};
	}, [key, path, paneId]);

	// ── Load ─────────────────────────────────────────────────────────────────
	useEffect(() => {
		let cancelled = false;
		setDoc(INITIAL);
		setMode('view');
		setSaveState({ kind: 'idle' });
		setConflict(null);
		setValidation(null);
		setNotice(null);
		lastSavedRef.current = null;
		fsRead(path)
			.then((res) => {
				if (cancelled) return;
				const dec = decodeForEdit(res.bytes);
				if (!dec.ok) {
					const text = decodeForView(res.bytes);
					baseRef.current = text;
					setDraft(text);
					setDoc({
						load: { kind: 'ready' },
						viewText: text,
						base: text,
						meta: INITIAL.meta,
						blocked: dec.reason,
					});
					return;
				}
				const stash = useEditingStore.getState().takeStash(path, paneId);
				if (stash) {
					// Unsaved edits from before this remount: back into Edit with
					// them, and raise the conflict choice if the file moved on.
					metaRef.current = stash.meta;
					baseRef.current = stash.base;
					setDraft(stash.draft);
					setDoc({
						load: { kind: 'ready' },
						viewText: dec.text,
						base: stash.base,
						meta: stash.meta,
						blocked: null,
					});
					setMode('edit');
					if (dec.text !== stash.base) setConflict({ kind: 'changed', theirs: dec.text });
					return;
				}
				metaRef.current = dec.meta;
				baseRef.current = dec.text;
				setDraft(dec.text);
				setDoc({
					load: { kind: 'ready' },
					viewText: dec.text,
					base: dec.text,
					meta: dec.meta,
					blocked: null,
				});
			})
			.catch((err) => {
				if (cancelled) return;
				setDoc({
					...INITIAL,
					load: { kind: 'error', message: err instanceof Error ? err.message : String(err) },
				});
			});
		return () => {
			cancelled = true;
		};
	}, [path, paneId, setDraft]);

	useEffect(
		() => () => {
			if (noticeTimer.current) clearTimeout(noticeTimer.current);
		},
		[]
	);

	// ── Save (conditional, F3) ───────────────────────────────────────────────
	const save = useCallback(
		async (opts?: { force?: boolean }) => {
			const force = opts?.force === true;
			if (doc.load.kind !== 'ready' || doc.blocked || savingRef.current) return;
			const next = draftRef.current;
			if (!force && next === baseRef.current) return;
			savingRef.current = true;
			setSaveState({ kind: 'saving' });
			try {
				if (validate) {
					const v = await validate(next);
					if (!v.ok) {
						setValidation(v);
						setSaveState({ kind: 'idle' });
						return;
					}
				}
				setValidation(null);
				if (!force) {
					let disk: string;
					try {
						const res = await fsRead(path);
						const dec = decodeForEdit(res.bytes, Number.POSITIVE_INFINITY);
						disk = dec.ok ? dec.text : normaliseText(decodeForView(res.bytes));
					} catch {
						setConflict({ kind: 'deleted' });
						setSaveState({ kind: 'idle' });
						return;
					}
					if (isConflict(baseRef.current, disk, lastSavedRef.current)) {
						setConflict({ kind: 'changed', theirs: disk });
						setSaveState({ kind: 'idle' });
						return;
					}
				}
				try {
					await fsWriteText(path, encodeForSave(next, metaRef.current));
				} catch (err) {
					setSaveState({
						kind: 'error',
						message: err instanceof Error ? err.message : String(err),
					});
					return;
				}
				lastSavedRef.current = next; // the watcher ignores our own write
				adoptBase(next);
				setConflict(null);
				setSaveState({ kind: 'idle' });
			} finally {
				savingRef.current = false;
			}
		},
		[doc.load.kind, doc.blocked, path, validate, adoptBase]
	);

	const keepMine = useCallback(() => save({ force: true }), [save]);

	const loadTheirs = useCallback(() => {
		if (conflict?.kind !== 'changed') return;
		adoptBase(conflict.theirs);
		setDraft(conflict.theirs);
		setConflict(null);
		setValidation(null);
		setSaveState({ kind: 'idle' });
	}, [conflict, adoptBase, setDraft]);

	const startEdit = useCallback(() => {
		if (doc.load.kind !== 'ready' || doc.blocked) return;
		setMode('edit');
	}, [doc.load.kind, doc.blocked]);

	const finishEdit = useCallback(() => {
		if (draftRef.current !== baseRef.current) return;
		setMode('view');
		setValidation(null);
		setSaveState({ kind: 'idle' });
	}, []);

	const cancel = useCallback(() => {
		setDraft(baseRef.current);
		setMode('view');
		setConflict(null);
		setValidation(null);
		setSaveState({ kind: 'idle' });
	}, [setDraft]);

	const dismissValidation = useCallback(() => setValidation(null), []);

	// ── Watch the file ───────────────────────────────────────────────────────
	const ready = doc.load.kind === 'ready';
	useEffect(() => {
		if (!ready) return;
		let active = true;
		let unlisten: UnlistenFn | undefined;
		let watcherId: string | undefined;
		void (async () => {
			try {
				const id = await fsWatch(path);
				if (!active) {
					void fsUnwatch(id);
					return;
				}
				watcherId = id;
				unlisten = await fsListenWatch(id, async () => {
					let disk: string;
					try {
						const res = await fsRead(path);
						const dec = decodeForEdit(res.bytes, Number.POSITIVE_INFINITY);
						disk = dec.ok ? dec.text : normaliseText(decodeForView(res.bytes));
					} catch {
						// Mid-write or deleted. A save re-reads and reports a
						// missing file then, so a transient miss is ignored here.
						return;
					}
					if (!active || savingRef.current) return;
					if (disk === lastSavedRef.current || disk === baseRef.current) return;
					if (draftRef.current === baseRef.current) {
						adoptBase(disk);
						setDraft(disk);
						if (modeRef.current === 'edit') flashNotice('Reloaded — the file changed on disk.');
						return;
					}
					setConflict({ kind: 'changed', theirs: disk });
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
	}, [ready, path, adoptBase, setDraft, flashNotice]);

	return {
		load: doc.load,
		viewText: doc.viewText,
		base: doc.base,
		draft,
		setDraft,
		dirty,
		mode,
		blocked: doc.blocked,
		saveState,
		conflict,
		validation,
		notice,
		startEdit,
		finishEdit,
		cancel,
		save,
		keepMine,
		loadTheirs,
		dismissValidation,
	};
}
