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
// - The file watcher never changes the buffer under Edit. In View (read-only,
//   nothing to lose) it reloads quietly; in Edit — clean or dirty — a change
//   on disk raises the conflict choice (Load theirs / Keep mine / Show diff).
//   Our own write is recognised and ignored.
// - Every draft remembers the base it descends from (its `Origin`), and Save
//   checks the file against *that* base, not merely the one on screen — so an
//   edit made on text the editor showed before a reload can never be written
//   over the newer file unseen (see "Lineage" below).
// - Text only ever enters the buffer (or becomes the save base) through the
//   strict `decodeForEdit` path — at load, and via `classifyReread` for every
//   later read (watcher, save re-read, Load theirs, Cancel). Bytes it refuses
//   (not UTF-8, binary, too large) block editing and Save; they are never
//   turned into lossy text that a save would write back over the file.
// - Unsaved edits survive an unmount (tab switch) through the editing store.
//
// Ordering. Every operation that reads the file and then changes the buffer —
// save (and Keep mine), Load theirs, Cancel/Discard, the watcher's reload and
// a `transform` such as Markdown's Format document — runs through one queue,
// one at a time, and `busy` names the one running so the UI can disable the
// others. On top of that, each operation notes the buffer generation when it
// starts (bumped whenever the base or draft is replaced by anything but
// typing) and applies its result only if the generation, and this mounted
// document, are still the ones it started from. Otherwise it drops the result
// (or, for a save, writes nothing and raises the conflict). Without this, a
// Save pressed during Load theirs wrote the discarded draft over the file, a
// format finishing after a watcher reload put the old text back over the new
// file, and so on — each async step applied a result computed against a base
// that had since changed.
//
// Lineage. Typing is the one buffer writer outside the queue. CodeMirror
// keeps its own copy of the text and takes a new `value` only in an effect
// after React renders, so a keystroke landing between a programmatic change
// and that render is computed on the text the editor still shows. Each draft
// therefore carries the `Origin` (base) of the text it was typed on: typing
// takes the origin of what the editor last rendered, not of the newest draft.
// Save compares the file against that origin's base, so such an edit meets
// the conflict choice instead of silently overwriting the change. A
// successful save links the old origin to the new one, so text typed while
// it ran counts as descending from what was written. This relies on the
// editor taking a new `value` in a passive effect, as @ikenga/ui-lib's
// CodeEditor does: that effect (in a child) runs just before the one here
// that records the origin, so the two always agree. An editor that showed a
// new value earlier would need to report its origin itself.

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

/** `theirs` is always strictly decoded text, never a lossy decode. It is
 *  shown in the diff, and Keep mine overwrites exactly that version — Load
 *  theirs re-reads the file. */
export type Conflict = { kind: 'changed'; theirs: string } | { kind: 'deleted' };

export type LoadState =
	| { kind: 'loading' }
	| { kind: 'ready' }
	| { kind: 'error'; message: string };

export type ValidationError = Extract<ValidationResult, { ok: false }>;

/** The buffer operations that run one at a time (see "Ordering" above). */
export type DocOp = 'save' | 'load-theirs' | 'cancel' | 'reread' | 'transform';

/** What became of a `transform`: applied, dropped because the buffer changed
 *  while it ran, or not run (nothing editable). */
export type TransformResult = 'applied' | 'stale' | 'skipped';

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
	/** The user's edits (typing). Programmatic changes go through `transform`. */
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
	/** The buffer operation running (or still queued), or null. Save, Keep
	 *  mine, Load theirs, Cancel and Format are disabled while it is set. */
	busy: DocOp | null;
	startEdit: () => void;
	/** Leave Edit. Only valid with no unsaved changes. */
	finishEdit: () => void;
	/** Discard the draft and show the file as it is on disk now (re-read
	 *  strictly), or say that it cannot be read. */
	cancel: () => Promise<void>;
	save: (opts?: { force?: boolean }) => Promise<void>;
	/** Conflict: overwrite the file with the draft. */
	keepMine: () => Promise<void>;
	/** Conflict: replace the draft with the file on disk (re-read strictly). */
	loadTheirs: () => Promise<void>;
	/** Replace the draft with `fn(draft)` (Markdown's Format document). The
	 *  result is dropped, not applied, if the buffer changed while `fn` ran —
	 *  a reload, or the user typing. Errors from `fn` reject. */
	transform: (fn: (text: string) => Promise<string>) => Promise<TransformResult>;
	dismissValidation: () => void;
}

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

/** Handed to each queued operation. */
interface OpContext {
	/** This mounted editor still shows the document the operation started on
	 *  (not unmounted, not switched to another file). */
	live: () => boolean;
	/** …and its base and draft have not been replaced since the operation
	 *  started. Only then may the operation apply its result. */
	current: () => boolean;
}

/** A base text a draft can descend from. Compared by identity: two loads of
 *  the same text are different origins. `savedAs` links an origin to the one a
 *  save from it produced, so text typed during the save follows it. */
interface Origin {
	base: string;
	savedAs: Origin | null;
}

const newOrigin = (base: string): Origin => ({ base, savedAs: null });

/** The newest origin on this line of descent. */
function resolveOrigin(o: Origin): Origin {
	let cur = o;
	while (cur.savedAs) cur = cur.savedAs;
	return cur;
}

const message = (err: unknown) => (err instanceof Error ? err.message : String(err));

export function useTextDocument({
	path,
	paneId = null,
	validate,
}: UseTextDocumentOptions): TextDocument {
	const [doc, setDoc] = useState<DocState>(INITIAL);
	// The buffer and the origin it descends from, committed together so the
	// effect below knows which origin the editor is showing.
	const [draftState, setDraftPair] = useState<{ text: string; origin: Origin }>(() => ({
		text: '',
		origin: newOrigin(''),
	}));
	const draft = draftState.text;
	const [mode, setMode] = useState<'view' | 'edit'>('view');
	const [saveState, setSaveState] = useState<SaveState>({ kind: 'idle' });
	const [conflict, setConflict] = useState<Conflict | null>(null);
	const [validation, setValidation] = useState<ValidationError | null>(null);
	const [busy, setBusy] = useState<DocOp | null>(null);

	// Refs the async paths read, so they see the latest values without
	// re-subscribing the watcher on every keystroke. Each is written together
	// with its state (setModeNow, raiseConflict, …), never by an effect after
	// the render — an unmount in between would read a stale value.
	const baseRef = useRef('');
	const draftRef = useRef('');
	const metaRef = useRef<DocumentMeta>(INITIAL.meta);
	const modeRef = useRef<'view' | 'edit'>('view');
	const conflictRef = useRef<Conflict | null>(null);
	// The document loaded (ready); operations do nothing before that.
	const loadedRef = useRef(false);
	// Mirrors `doc.blocked` for the async paths (watcher, save, Load theirs).
	const blockedRef = useRef<string | null>(null);
	// A stashed draft claimed when the load started and not yet put back in
	// the buffer. If the editor unmounts first, it goes back into the store.
	const pendingStashRef = useRef<StashedDraft | null>(null);
	// A draft stashed before this remount that cannot be applied to the file
	// as it is now (too large, binary, not UTF-8). Held, never dropped: the
	// session keeps reporting it as unsaved, and the next unmount stashes it
	// again.
	const heldStashRef = useRef<StashedDraft | null>(null);
	// The load failed and a stashed draft was restored in its place.
	const loadErrorRef = useRef<string | null>(null);
	// Lineage (see the header): the origin of `baseRef`, of `draftRef`, and of
	// the text the editor last rendered — which is what a keystroke is typed on.
	const baseOriginRef = useRef<Origin>(newOrigin(''));
	const draftOriginRef = useRef<Origin>(baseOriginRef.current);
	const editorOriginRef = useRef<Origin>(baseOriginRef.current);

	// Ordering (see the header): `epoch` changes when this editor stops
	// showing the document (unmount, another file); `gen` whenever the base or
	// draft is replaced other than by typing. The queue runs operations one
	// at a time.
	const epochRef = useRef(0);
	const genRef = useRef(0);
	const queueRef = useRef<Promise<unknown>>(Promise.resolve());
	const queuedOpsRef = useRef(0);
	const rereadRef = useRef<Promise<unknown> | null>(null);

	const dirty = doc.load.kind === 'ready' && draft !== doc.base;
	const key = sessionKey(path, paneId);

	// The editor has rendered `draftState`: its CodeMirror effect (a child,
	// so it runs first) has synced the text, and a keystroke from here on is
	// typed on text descending from this origin.
	useEffect(() => {
		editorOriginRef.current = draftState.origin;
	}, [draftState]);

	/** Set the buffer and the origin it descends from. */
	const putDraft = useCallback((text: string, origin: Origin) => {
		draftRef.current = text;
		draftOriginRef.current = origin;
		setDraftPair({ text, origin });
	}, []);

	/** The user typing. Does not bump the generation: a save may run while
	 *  the user types (it saves what it read), and a transform checks the
	 *  draft itself. The text was typed on what the editor last rendered, so
	 *  it descends from that origin — not necessarily the newest draft's. */
	const setDraft = useCallback(
		(next: string) => {
			if (next === draftRef.current) return;
			putDraft(next, editorOriginRef.current);
		},
		[putDraft]
	);

	/** Replace the draft programmatically (reload, Load theirs, Cancel, a
	 *  restored stash, a transform) with text descending from `origin`. */
	const replaceDraft = useCallback(
		(next: string, origin: Origin) => {
			genRef.current++;
			putDraft(next, origin);
		},
		[putDraft]
	);

	/** Take strictly decoded disk text (and its line ending / BOM) as the
	 *  base — and as the draft too, when `withDraft`. Returns the new base's
	 *  origin. */
	const adoptDisk = useCallback(
		(text: string, meta: DocumentMeta, withDraft: boolean): Origin => {
			genRef.current++;
			const origin = newOrigin(text);
			metaRef.current = meta;
			baseRef.current = text;
			baseOriginRef.current = origin;
			if (withDraft) replaceDraft(text, origin);
			setDoc((d) => ({ ...d, base: text, viewText: text, meta }));
			return origin;
		},
		[replaceDraft]
	);

	const setModeNow = useCallback((next: 'view' | 'edit') => {
		modeRef.current = next;
		setMode(next);
	}, []);

	const raiseConflict = useCallback((next: Conflict | null) => {
		conflictRef.current = next;
		setConflict(next);
	}, []);

	/** The file on disk can no longer be edited. Nothing lossy enters the
	 *  buffer: the base and a dirty draft are kept as they are, View shows the
	 *  lossy text, and Save refuses until the file is editable again. A clean
	 *  buffer leaves Edit. */
	const blockEditing = useCallback(
		(reason: string, viewText: string) => {
			const unsaved = modeRef.current === 'edit' && draftRef.current !== baseRef.current;
			blockedRef.current = reason;
			raiseConflict(null);
			setValidation(null);
			setSaveState({ kind: 'idle' });
			setDoc((d) => ({ ...d, viewText, blocked: reason }));
			if (!unsaved) setModeNow('view');
		},
		[raiseConflict, setModeNow]
	);

	/** The file is editable text again (a held stash keeps it blocked). */
	const unblockEditing = useCallback(() => {
		if (blockedRef.current === null || heldStashRef.current) return;
		blockedRef.current = null;
		setDoc((d) => ({ ...d, blocked: null }));
	}, []);

	/**
	 * Queue a buffer operation behind the ones already queued. It is skipped
	 * if this editor stopped showing the document before its turn came. `fn`
	 * reads the refs when it runs — never state captured when it was queued.
	 */
	const runOp = useCallback(
		<T>(kind: DocOp, fn: (op: OpContext) => Promise<T>): Promise<T | undefined> => {
			const epoch = epochRef.current;
			const live = () => epoch === epochRef.current;
			queuedOpsRef.current++;
			setBusy((b) => b ?? kind);
			const run = async (): Promise<T | undefined> => {
				if (!live()) return undefined;
				const gen = genRef.current;
				setBusy(kind);
				try {
					return await fn({ live, current: () => live() && gen === genRef.current });
				} finally {
					if (live()) {
						queuedOpsRef.current--;
						if (queuedOpsRef.current === 0) setBusy(null);
					}
				}
			};
			const result = queueRef.current.then(run);
			queueRef.current = result.catch(() => undefined);
			return result;
		},
		[]
	);

	// ── Session registration (editing store) ────────────────────────────────
	const upsert = useEditingStore((s) => s.upsert);
	useEffect(() => {
		upsert(key, { path, paneId, mounted: true, editing: mode === 'edit', dirty });
	}, [upsert, key, path, paneId, mode, dirty]);
	// Declared before the load effect, so on unmount this runs first, while a
	// stash the load has not consumed yet is still in `pendingStashRef`.
	useEffect(() => {
		return () => {
			const store = useEditingStore.getState();
			const cur = store.sessions[key];
			const pending = pendingStashRef.current;
			pendingStashRef.current = null;
			if (cur?.discarded) {
				store.remove(key);
				return;
			}
			const unsaved = modeRef.current === 'edit' && draftRef.current !== baseRef.current;
			// Never drop a session that still holds a stash — one claimed by a
			// load that has not finished, one held for a file that cannot be
			// edited, or any other.
			// The stash is based on what the draft descends from, so the
			// remount's conflict check compares against the right text.
			const stash: StashedDraft | undefined = unsaved
				? {
						draft: draftRef.current,
						base: resolveOrigin(draftOriginRef.current).base,
						meta: metaRef.current,
					}
				: (heldStashRef.current ?? pending ?? cur?.stash);
			if (!stash) {
				store.remove(key);
				return;
			}
			store.upsert(key, {
				path,
				paneId,
				mounted: false,
				editing: false,
				dirty: true,
				stash,
			});
		};
	}, [key, path, paneId]);

	// ── Load ─────────────────────────────────────────────────────────────────
	useEffect(() => {
		const epoch = ++epochRef.current;
		const live = () => epoch === epochRef.current;
		const sKey = sessionKey(path, paneId);
		genRef.current++;
		queueRef.current = Promise.resolve();
		queuedOpsRef.current = 0;
		rereadRef.current = null;
		setBusy(null);
		loadedRef.current = false;
		baseRef.current = '';
		baseOriginRef.current = newOrigin('');
		putDraft('', baseOriginRef.current);
		metaRef.current = INITIAL.meta;
		setDoc(INITIAL);
		setModeNow('view');
		setSaveState({ kind: 'idle' });
		raiseConflict(null);
		setValidation(null);
		heldStashRef.current = null;
		loadErrorRef.current = null;
		blockedRef.current = null;
		// Claim any stashed draft now, before the read: from here on this
		// editor owns it, and an unmount before the read returns stashes it
		// again (the cleanup above) instead of losing it.
		pendingStashRef.current = useEditingStore.getState().claimStash(sKey, path, paneId);

		/** The claimed stash, unless the user discarded it (close guard) while
		 *  the file was loading. */
		const takeClaimed = (): StashedDraft | null => {
			const s = pendingStashRef.current;
			pendingStashRef.current = null;
			if (!s || useEditingStore.getState().sessions[sKey]?.discarded) return null;
			return s;
		};
		/** Put a stashed draft back in the buffer, in Edit. */
		const restore = (stash: StashedDraft, viewText: string) => {
			genRef.current++;
			metaRef.current = stash.meta;
			baseRef.current = stash.base;
			baseOriginRef.current = newOrigin(stash.base);
			putDraft(stash.draft, baseOriginRef.current);
			setDoc({
				load: { kind: 'ready' },
				viewText,
				base: stash.base,
				meta: stash.meta,
				blocked: null,
				held: false,
			});
			setModeNow('edit');
			loadedRef.current = true;
			// The buffer holds the draft now; the session reports it as dirty.
			useEditingStore.getState().upsert(sKey, {
				path,
				paneId,
				mounted: true,
				editing: true,
				dirty: stash.draft !== stash.base,
				stash: undefined,
			});
		};

		fsRead(path).then(
			(res) => {
				if (!live()) return;
				const stash = takeClaimed();
				const dec = decodeForEdit(res.bytes);
				if (!dec.ok) {
					// The lossy text is for View only; it never becomes the
					// base or the buffer.
					blockedRef.current = dec.reason;
					if (stash) {
						// The draft cannot go into an editor for this file now.
						// Keep it — still counted as unsaved by the close guard
						// and the reload prompt — and say so.
						heldStashRef.current = stash;
						useEditingStore.getState().upsert(sKey, { path, paneId, mounted: true, stash });
					}
					setDoc({
						load: { kind: 'ready' },
						viewText: decodeForView(res.bytes),
						base: '',
						meta: INITIAL.meta,
						blocked: dec.reason,
						held: stash !== null,
					});
					loadedRef.current = true;
					return;
				}
				if (stash) {
					// Unsaved edits from before this remount: back into Edit with
					// them, and raise the conflict choice if the file moved on.
					restore(stash, dec.text);
					if (dec.text !== stash.base) raiseConflict({ kind: 'changed', theirs: dec.text });
					return;
				}
				genRef.current++;
				metaRef.current = dec.meta;
				baseRef.current = dec.text;
				baseOriginRef.current = newOrigin(dec.text);
				putDraft(dec.text, baseOriginRef.current);
				setDoc({
					load: { kind: 'ready' },
					viewText: dec.text,
					base: dec.text,
					meta: dec.meta,
					blocked: null,
					held: false,
				});
				loadedRef.current = true;
			},
			(err) => {
				if (!live()) return;
				const stash = takeClaimed();
				if (stash) {
					// The file is gone (moved, trashed, or unreadable) but there
					// are unsaved edits for it: back into Edit with them and the
					// "moved or deleted" choice, never a silent drop.
					loadErrorRef.current = message(err);
					restore(stash, stash.base);
					raiseConflict({ kind: 'deleted' });
					return;
				}
				setDoc({ ...INITIAL, load: { kind: 'error', message: message(err) } });
			}
		);
		return () => {
			// Anything still running or queued for this document is dropped,
			// and nothing new starts on it.
			epochRef.current++;
			loadedRef.current = false;
		};
	}, [path, paneId, setModeNow, raiseConflict, putDraft]);

	// ── Save (conditional, F3) ───────────────────────────────────────────────
	const save = useCallback(
		async (opts?: { force?: boolean }) => {
			await runOp('save', async (op) => {
				if (!loadedRef.current || blockedRef.current !== null) return;
				// A forced save answers the conflict on screen now; with none
				// (resolved meanwhile) it is an ordinary conditional save.
				const answered = opts?.force === true ? conflictRef.current : null;
				const next = draftRef.current;
				// The base `next` was typed on — normally the one on screen, but
				// not if the buffer changed under an edit (see "Lineage"). The
				// file is checked against this one.
				const origin = resolveOrigin(draftOriginRef.current);
				const base = origin.base;
				let meta = metaRef.current;
				if (answered === null && next === baseRef.current) return;
				setSaveState({ kind: 'saving' });
				let outcome: SaveState = { kind: 'idle' };
				try {
					if (validate) {
						const v = await validate(next);
						if (!v.ok) {
							if (op.live()) setValidation(v);
							return;
						}
					}
					if (op.live()) setValidation(null);
					// Re-read even for a forced save: "Keep mine" / "Save anyway"
					// overwrite the version the user was shown, or a missing file,
					// by choice — never a newer one, and never a file that is no
					// longer editable text.
					let bytes: number[] | null = null;
					try {
						bytes = (await fsRead(path)).bytes;
					} catch {
						bytes = null;
					}
					if (op.live() && !op.current()) {
						// The base was replaced while this save ran. `next` was
						// made against the old one: write nothing, and show the
						// conflict if the file differs from the base now shown.
						const disk = bytes ? classifyReread(bytes, baseRef.current) : null;
						if (disk?.kind === 'changed') raiseConflict({ kind: 'changed', theirs: disk.text });
						return;
					}
					if (bytes === null) {
						// Only "Save anyway (recreate it)" writes a missing file.
						if (answered?.kind !== 'deleted') {
							if (op.live()) raiseConflict({ kind: 'deleted' });
							return;
						}
					} else {
						const disk = classifyReread(bytes, base);
						if (disk.kind === 'refused') {
							if (op.live()) blockEditing(disk.reason, disk.viewText);
							return;
						}
						// Keep mine overwrites exactly the "theirs" it was shown.
						// A file that changed again — or one that came back after
						// "Save anyway (recreate it)" was offered — is a new
						// conflict, not something to overwrite unseen.
						if (
							disk.kind === 'changed' &&
							!(answered?.kind === 'changed' && answered.theirs === disk.text)
						) {
							if (op.live()) raiseConflict({ kind: 'changed', theirs: disk.text });
							return;
						}
						// Same text on disk: write it back in the file's current
						// line ending and BOM, not the ones it had when opened.
						if (disk.kind === 'unchanged') meta = disk.meta;
					}
					if (op.live() && (blockedRef.current !== null || !op.current())) return;
					try {
						await fsWriteText(path, encodeForSave(next, meta));
					} catch (err) {
						outcome = { kind: 'error', message: message(err) };
						return;
					}
					if (!op.live()) {
						// The editor unmounted while the write was under way; its
						// draft is stashed on the old base. Move it onto the write.
						useEditingStore.getState().rebaseStash(key, base, next, meta);
						return;
					}
					if (!op.current()) return;
					// The base is now what we wrote, so the watcher's `disk ===
					// base` check ignores our own write. Text typed on `origin`
					// meanwhile now descends from the write.
					loadErrorRef.current = null;
					origin.savedAs = adoptDisk(next, meta, false);
					raiseConflict(null);
				} finally {
					if (op.live()) setSaveState(outcome);
				}
			});
		},
		[runOp, path, key, validate, adoptDisk, blockEditing, raiseConflict]
	);

	const keepMine = useCallback(() => save({ force: true }), [save]);

	const loadTheirs = useCallback(async () => {
		await runOp('load-theirs', async (op) => {
			if (!loadedRef.current || conflictRef.current?.kind !== 'changed') return;
			// Read the file now rather than trusting `conflict.theirs`: it may
			// have changed again — possibly into bytes that cannot be edited.
			let bytes: number[];
			try {
				bytes = (await fsRead(path)).bytes;
			} catch {
				if (op.current()) raiseConflict({ kind: 'deleted' });
				return;
			}
			if (!op.current()) return;
			const disk = classifyReread(bytes, baseRef.current);
			if (disk.kind === 'refused') {
				blockEditing(disk.reason, disk.viewText);
				return;
			}
			const text = disk.kind === 'changed' ? disk.text : baseRef.current;
			adoptDisk(text, disk.meta, true);
			unblockEditing();
			raiseConflict(null);
			setValidation(null);
			setSaveState({ kind: 'idle' });
		});
	}, [runOp, path, adoptDisk, blockEditing, unblockEditing, raiseConflict]);

	const cancel = useCallback(async () => {
		await runOp('cancel', async (op) => {
			if (!loadedRef.current) return;
			// Discard means "show me the file": re-read it rather than going
			// back to the base, which an outside write may have left behind.
			let bytes: number[] | null = null;
			let readError = '';
			try {
				bytes = (await fsRead(path)).bytes;
			} catch (err) {
				readError = message(err);
			}
			if (!op.current()) return;
			loadErrorRef.current = null;
			if (heldStashRef.current) {
				heldStashRef.current = null;
				useEditingStore
					.getState()
					.upsert(sessionKey(path, paneId), { path, paneId, stash: undefined });
			}
			raiseConflict(null);
			setValidation(null);
			setSaveState({ kind: 'idle' });
			setModeNow('view');
			if (bytes === null) {
				// Nothing to show but why: the file cannot be read.
				blockedRef.current = null;
				loadedRef.current = false;
				adoptDisk('', INITIAL.meta, true);
				setDoc({ ...INITIAL, load: { kind: 'error', message: readError } });
				return;
			}
			const disk = classifyReread(bytes, baseRef.current);
			if (disk.kind === 'refused') {
				// As at load: the lossy text is shown, never edited.
				adoptDisk('', INITIAL.meta, true);
				blockedRef.current = disk.reason;
				setDoc((d) => ({ ...d, viewText: disk.viewText, blocked: disk.reason, held: false }));
				return;
			}
			adoptDisk(disk.kind === 'changed' ? disk.text : baseRef.current, disk.meta, true);
			blockedRef.current = null;
			setDoc((d) => ({ ...d, blocked: null, held: false }));
		});
	}, [runOp, path, paneId, adoptDisk, raiseConflict, setModeNow]);

	const transform = useCallback(
		async (fn: (text: string) => Promise<string>): Promise<TransformResult> => {
			const result = await runOp('transform', async (op): Promise<TransformResult> => {
				if (!loadedRef.current || blockedRef.current !== null || modeRef.current !== 'edit') {
					return 'skipped';
				}
				const src = draftRef.current;
				const out = await fn(src);
				// Computed from `src`: apply it only to that same buffer.
				if (!op.current() || draftRef.current !== src || blockedRef.current !== null) {
					return 'stale';
				}
				// Formatted text descends from the same base as its source.
				if (out !== src) replaceDraft(out, draftOriginRef.current);
				return 'applied';
			});
			return result ?? 'stale';
		},
		[runOp, replaceDraft]
	);

	const startEdit = useCallback(() => {
		if (doc.load.kind !== 'ready' || doc.blocked !== null) return;
		setModeNow('edit');
	}, [doc.load.kind, doc.blocked, setModeNow]);

	const dismissValidation = useCallback(() => setValidation(null), []);

	// ── Watch the file ───────────────────────────────────────────────────────
	/** Re-read after a change event, queued like any other operation (so it
	 *  waits for a save, a format, … to finish). Events while one is already
	 *  waiting share it. */
	const reread = useCallback(() => {
		if (rereadRef.current) return rereadRef.current;
		const pending = runOp('reread', async (op) => {
			rereadRef.current = null;
			if (!loadedRef.current) return;
			let bytes: number[];
			try {
				bytes = (await fsRead(path)).bytes;
			} catch {
				// Mid-write or deleted. A save re-reads and reports a missing
				// file then, so a transient miss is ignored here.
				return;
			}
			if (!op.current()) return;
			// No "our last write" exception (see isConflict): after a save the
			// base already is our write.
			const disk = classifyReread(bytes, baseRef.current);
			if (disk.kind === 'refused') {
				blockEditing(disk.reason, disk.viewText);
				return;
			}
			// Only View takes a change quietly: nothing there can be lost. In
			// Edit the buffer is never replaced under the user, clean or dirty
			// — a keystroke can always be on its way (see "Lineage").
			const viewing = modeRef.current === 'view' && draftRef.current === baseRef.current;
			if (disk.kind === 'unchanged') {
				// Same text — maybe new line endings or BOM. View takes them; in
				// Edit the save re-read picks up the file's current ones.
				if (viewing) adoptDisk(baseRef.current, disk.meta, false);
				else setDoc((d) => ({ ...d, viewText: baseRef.current }));
				unblockEditing();
				return;
			}
			unblockEditing();
			if (viewing) {
				adoptDisk(disk.text, disk.meta, true);
				return;
			}
			raiseConflict({ kind: 'changed', theirs: disk.text });
		});
		rereadRef.current = pending;
		return pending;
	}, [runOp, path, adoptDisk, blockEditing, unblockEditing, raiseConflict]);

	const finishEdit = useCallback(() => {
		if (draftRef.current !== baseRef.current) return;
		setModeNow('view');
		setValidation(null);
		setSaveState({ kind: 'idle' });
		// Leaving Edit with the "changed on disk" choice still open on a clean
		// buffer: nothing to keep, so View shows the file as it is now.
		if (conflictRef.current?.kind === 'changed') {
			raiseConflict(null);
			void reread();
		}
	}, [setModeNow, raiseConflict, reread]);

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
				unlisten = await fsListenWatch(id, () => (active ? reread() : undefined));
			} catch {
				/* watching is best-effort */
			}
		})();
		return () => {
			active = false;
			unlisten?.();
			if (watcherId) void fsUnwatch(watcherId);
		};
	}, [ready, path, reread]);

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
		busy,
		startEdit,
		finishEdit,
		cancel,
		save,
		keepMine,
		loadTheirs,
		transform,
		dismissValidation,
	};
}
