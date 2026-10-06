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
// - Text only ever enters the buffer (or becomes the save base) through the
//   strict `decodeForEdit` path — at load, and via `classifyReread` for every
//   later read (watcher, save re-read, Load theirs). Bytes it refuses (not
//   UTF-8, binary, too large) block editing and Save; they are never turned
//   into lossy text that a save would write back over the file.
// - Unsaved edits survive an unmount (tab switch) through the editing store.

import { useCallback, useEffect, useRef, useState } from 'react';
import { type StashedDraft, sessionKey, useEditingStore } from '@/lib/editing/editing-store';
import { fsListenWatch, fsRead, fsUnwatch, fsWatch, fsWriteText } from '@/lib/tauri-cmd';
import type { UnlistenFn } from '@/lib/transport';
import {
	classifyReread,
	type DocumentMeta,
	decodeForEdit,
	decodeForView,
	encodeForSave,
} from './text-document';
import type { ValidationResult, Validator } from './validate';

export type SaveState = { kind: 'idle' } | { kind: 'saving' } | { kind: 'error'; message: string };

/** `theirs` is always strictly decoded text, never a lossy decode; it is
 *  shown in the diff only — Load theirs re-reads the file. */
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
	/** Why Edit (and Save) is unavailable — too large, binary, not UTF-8 —
	 *  or null. Can be set while editing, when the file changes into one of
	 *  those on disk; a dirty draft is then kept but cannot be saved. */
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
	/** Conflict: replace the draft with the file on disk (re-read strictly). */
	loadTheirs: () => Promise<void>;
	dismissValidation: () => void;
}

const NOTICE_MS = 4_000;

const HELD_NOTE = 'Your unsaved edits are kept but can’t be applied to the file as it is now.';
const UNSAVED_NOTE =
	'Your unsaved edits are kept, but Save is off until the file is editable text again.';

interface DocState {
	load: LoadState;
	viewText: string;
	base: string;
	meta: DocumentMeta;
	/** The reason editing is blocked (no notes appended), or null. */
	blocked: string | null;
	/** A stashed draft from before this mount is held, not applied. */
	held: boolean;
}

const INITIAL: DocState = {
	load: { kind: 'loading' },
	viewText: '',
	base: '',
	meta: { eol: '\n', bom: false },
	blocked: null,
	held: false,
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
	// A draft stashed before this remount that cannot be applied to the file
	// as it is now (too large, binary, not UTF-8). Held, never dropped: the
	// session keeps reporting it as unsaved, and the next unmount stashes it
	// again.
	const heldStashRef = useRef<StashedDraft | null>(null);
	// The load failed and a stashed draft was restored in its place: Cancel
	// returns to that error instead of showing a file that is not there.
	const loadErrorRef = useRef<string | null>(null);
	const modeRef = useRef<'view' | 'edit'>('view');
	// Mirrors `doc.blocked` for the async paths (watcher, save, Load theirs).
	const blockedRef = useRef<string | null>(null);
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

	/** Take strictly decoded disk text (and its line ending / BOM) as the base. */
	const adoptDisk = useCallback((text: string, meta: DocumentMeta) => {
		metaRef.current = meta;
		baseRef.current = text;
		setDoc((d) => ({ ...d, base: text, viewText: text, meta }));
	}, []);

	/** The file on disk can no longer be edited. Nothing lossy enters the
	 *  buffer: the base and a dirty draft are kept as they are, View shows the
	 *  lossy text, and Save refuses until the file is editable again. A clean
	 *  buffer leaves Edit. */
	const blockEditing = useCallback((reason: string, viewText: string) => {
		const unsaved = modeRef.current === 'edit' && draftRef.current !== baseRef.current;
		blockedRef.current = reason;
		setConflict(null);
		setValidation(null);
		setSaveState({ kind: 'idle' });
		setDoc((d) => ({ ...d, viewText, blocked: reason }));
		if (!unsaved) setMode('view');
	}, []);

	/** The file is editable text again (a held stash keeps it blocked). */
	const unblockEditing = useCallback(() => {
		if (blockedRef.current === null || heldStashRef.current) return;
		blockedRef.current = null;
		setDoc((d) => ({ ...d, blocked: null }));
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
			const held = heldStashRef.current;
			if (cur?.discarded || (!unsaved && !held)) {
				store.remove(key);
				return;
			}
			store.upsert(key, {
				path,
				paneId,
				mounted: false,
				editing: false,
				dirty: true,
				stash: unsaved
					? { draft: draftRef.current, base: baseRef.current, meta: metaRef.current }
					: (held ?? undefined),
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
		heldStashRef.current = null;
		loadErrorRef.current = null;
		blockedRef.current = null;
		fsRead(path)
			.then((res) => {
				if (cancelled) return;
				// Take any stashed draft before deciding how to open the file, so
				// no branch below can leave it behind unseen (and then drop it on
				// the next unmount).
				const stash = useEditingStore.getState().takeStash(path, paneId);
				const dec = decodeForEdit(res.bytes);
				if (!dec.ok) {
					// The lossy text is for View only; it never becomes the
					// base or the buffer.
					const text = decodeForView(res.bytes);
					baseRef.current = '';
					metaRef.current = INITIAL.meta;
					setDraft('');
					blockedRef.current = dec.reason;
					if (stash) {
						// The draft cannot go into an editor for this file now.
						// Keep it — still counted as unsaved by the close guard
						// and the reload prompt — and say so.
						heldStashRef.current = stash;
						useEditingStore
							.getState()
							.upsert(sessionKey(path, paneId), { path, paneId, mounted: true, stash });
					}
					setDoc({
						load: { kind: 'ready' },
						viewText: text,
						base: '',
						meta: INITIAL.meta,
						blocked: dec.reason,
						held: stash !== null,
					});
					return;
				}
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
						held: false,
					});
					setMode('edit');
					if (dec.text !== stash.base) {
						setConflict({ kind: 'changed', theirs: dec.text });
					}
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
					held: false,
				});
			})
			.catch((err) => {
				if (cancelled) return;
				const message = err instanceof Error ? err.message : String(err);
				const stash = useEditingStore.getState().takeStash(path, paneId);
				if (stash) {
					// The file is gone (moved, trashed, or unreadable) but there
					// are unsaved edits for it: back into Edit with them and the
					// "moved or deleted" choice, never a silent drop.
					loadErrorRef.current = message;
					metaRef.current = stash.meta;
					baseRef.current = stash.base;
					setDraft(stash.draft);
					setDoc({
						load: { kind: 'ready' },
						viewText: stash.base,
						base: stash.base,
						meta: stash.meta,
						blocked: null,
						held: false,
					});
					setMode('edit');
					setConflict({ kind: 'deleted' });
					return;
				}
				setDoc({ ...INITIAL, load: { kind: 'error', message } });
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
			if (doc.load.kind !== 'ready' || blockedRef.current !== null || savingRef.current) return;
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
				// Re-read even for a forced save: "Keep mine" / "Save anyway"
				// overwrite a newer or missing file by choice, but never one
				// that is no longer editable text.
				let bytes: number[] | null = null;
				try {
					bytes = (await fsRead(path)).bytes;
				} catch {
					if (!force) {
						setConflict({ kind: 'deleted' });
						setSaveState({ kind: 'idle' });
						return;
					}
				}
				if (bytes) {
					const disk = classifyReread(bytes, baseRef.current);
					if (disk.kind === 'refused') {
						blockEditing(disk.reason, disk.viewText);
						return;
					}
					if (!force && disk.kind === 'changed') {
						setConflict({ kind: 'changed', theirs: disk.text });
						setSaveState({ kind: 'idle' });
						return;
					}
					// Same text on disk: write it back in the file's current line
					// ending and BOM, not the ones it had when it was opened.
					if (disk.kind === 'unchanged') metaRef.current = disk.meta;
				}
				// A Load theirs that resolved during the awaits above may have
				// blocked editing.
				if (blockedRef.current !== null) {
					setSaveState({ kind: 'idle' });
					return;
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
				// The base is now what we wrote, so the watcher's `disk === base`
				// check ignores our own write.
				loadErrorRef.current = null;
				adoptBase(next);
				setConflict(null);
				setSaveState({ kind: 'idle' });
			} finally {
				savingRef.current = false;
			}
		},
		[doc.load.kind, path, validate, adoptBase, blockEditing]
	);

	const keepMine = useCallback(() => save({ force: true }), [save]);

	const loadTheirs = useCallback(async () => {
		if (conflict?.kind !== 'changed' || savingRef.current) return;
		// Read the file now rather than trusting `conflict.theirs`: it may have
		// changed again — possibly into bytes that cannot be edited.
		let bytes: number[];
		try {
			bytes = (await fsRead(path)).bytes;
		} catch {
			setConflict({ kind: 'deleted' });
			return;
		}
		const disk = classifyReread(bytes, baseRef.current);
		if (disk.kind === 'refused') {
			blockEditing(disk.reason, disk.viewText);
			return;
		}
		const text = disk.kind === 'changed' ? disk.text : baseRef.current;
		adoptDisk(text, disk.meta);
		setDraft(text);
		unblockEditing();
		setConflict(null);
		setValidation(null);
		setSaveState({ kind: 'idle' });
	}, [conflict, path, adoptDisk, setDraft, blockEditing, unblockEditing]);

	const startEdit = useCallback(() => {
		if (doc.load.kind !== 'ready' || doc.blocked !== null) return;
		setMode('edit');
	}, [doc.load.kind, doc.blocked]);

	const finishEdit = useCallback(() => {
		if (draftRef.current !== baseRef.current) return;
		setMode('view');
		setValidation(null);
		setSaveState({ kind: 'idle' });
	}, []);

	const cancel = useCallback(() => {
		const loadError = loadErrorRef.current;
		if (loadError !== null) {
			// The draft stood in for a file that could not be read; discarding
			// it leaves nothing to view but that error.
			loadErrorRef.current = null;
			blockedRef.current = null;
			baseRef.current = '';
			setDraft('');
			setMode('view');
			setConflict(null);
			setValidation(null);
			setSaveState({ kind: 'idle' });
			setDoc({ ...INITIAL, load: { kind: 'error', message: loadError } });
			return;
		}
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
					let bytes: number[];
					try {
						bytes = (await fsRead(path)).bytes;
					} catch {
						// Mid-write or deleted. A save re-reads and reports a
						// missing file then, so a transient miss is ignored here.
						return;
					}
					if (!active || savingRef.current) return;
					// No "our last write" exception (see isConflict): after a save
					// the base already is our write.
					const disk = classifyReread(bytes, baseRef.current);
					if (disk.kind === 'refused') {
						blockEditing(disk.reason, disk.viewText);
						return;
					}
					const clean = draftRef.current === baseRef.current;
					if (disk.kind === 'unchanged') {
						// Same text — maybe new line endings or BOM, which a clean
						// buffer takes so the next save writes what is on disk.
						if (clean) adoptDisk(baseRef.current, disk.meta);
						else setDoc((d) => ({ ...d, viewText: baseRef.current }));
						unblockEditing();
						return;
					}
					unblockEditing();
					if (clean) {
						adoptDisk(disk.text, disk.meta);
						setDraft(disk.text);
						if (modeRef.current === 'edit') flashNotice('Reloaded — the file changed on disk.');
						return;
					}
					setConflict({ kind: 'changed', theirs: disk.text });
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
	}, [ready, path, adoptDisk, setDraft, flashNotice, blockEditing, unblockEditing]);

	let blocked = doc.blocked;
	if (blocked !== null && doc.held) blocked = `${blocked} ${HELD_NOTE}`;
	else if (blocked !== null && mode === 'edit' && dirty) blocked = `${blocked} ${UNSAVED_NOTE}`;

	return {
		load: doc.load,
		viewText: doc.viewText,
		base: doc.base,
		draft,
		setDraft,
		dirty,
		mode,
		blocked,
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
