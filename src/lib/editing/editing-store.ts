// Open text-editing sessions, one per (pane, file) — plans/file-editing Shape 1.
//
// Three readers:
// - `ArtifactView` freezes its disk-watch remount key while a session for its
//   file is in Edit, so our own save (or an outside write) does not remount the
//   editor and throw the user out of Edit or drop the draft.
// - The unsaved-changes guard (`lib/panes/unsaved-guard.ts`) asks which tabs
//   being closed hold unsaved edits.
// - A remounted editor takes back a stashed draft. A pane mounts only its
//   active tab, so switching tabs (or Open source / Close source) unmounts the
//   editor; an unmount with unsaved edits stashes the draft here instead of
//   losing it.
//
// Client state only, never persisted: a reload or app restart is covered by
// the `beforeunload` prompt below, not by restoring drafts.

import { create } from 'zustand';

export interface StashedDraft {
	draft: string;
	/** The disk text the draft was based on — a remount compares it with the
	 *  file as it is now, to raise the conflict choice if it moved on. */
	base: string;
	meta: { eol: '\n' | '\r\n'; bom: boolean };
}

export interface EditSession {
	path: string;
	paneId: string | null;
	/** A live editor component owns this session. */
	mounted: boolean;
	/** The editor is in Edit (not View). */
	editing: boolean;
	dirty: boolean;
	/** Set while unmounted with unsaved edits. */
	stash?: StashedDraft;
	/** The user chose to discard (close guard). An unmount must not stash. */
	discarded?: boolean;
}

interface EditingState {
	sessions: Record<string, EditSession>;
	upsert: (key: string, patch: Partial<EditSession> & Pick<EditSession, 'path' | 'paneId'>) => void;
	remove: (key: string) => void;
	/** Mark sessions discarded and drop any stash they hold. */
	discard: (keys: string[]) => void;
	/** Take (and clear) the stashed draft for this file: this pane's first,
	 *  else any unmounted stash for the same path (the tab moved panes). */
	takeStash: (path: string, paneId: string | null) => StashedDraft | null;
}

export function sessionKey(path: string, paneId: string | null | undefined): string {
	return `${paneId ?? ''}\u0000${path}`;
}

export const useEditingStore = create<EditingState>((set, get) => ({
	sessions: {},
	upsert: (key, patch) =>
		set((s) => {
			const prev = s.sessions[key];
			const next: EditSession = {
				...(prev ?? { mounted: false, editing: false, dirty: false }),
				...patch,
			};
			if (
				prev &&
				prev.mounted === next.mounted &&
				prev.editing === next.editing &&
				prev.dirty === next.dirty &&
				prev.stash === next.stash &&
				prev.discarded === next.discarded
			) {
				return s;
			}
			return { sessions: { ...s.sessions, [key]: next } };
		}),
	remove: (key) =>
		set((s) => {
			if (!(key in s.sessions)) return s;
			const { [key]: _gone, ...rest } = s.sessions;
			return { sessions: rest };
		}),
	discard: (keys) =>
		set((s) => {
			const sessions = { ...s.sessions };
			for (const k of keys) {
				const cur = sessions[k];
				if (!cur) continue;
				if (cur.mounted) sessions[k] = { ...cur, discarded: true, stash: undefined, dirty: false };
				else delete sessions[k];
			}
			return { sessions };
		}),
	takeStash: (path, paneId) => {
		const { sessions } = get();
		let key = sessionKey(path, paneId);
		if (!sessions[key]?.stash) {
			const other = Object.entries(sessions).find(
				([, v]) => v.path === path && !v.mounted && v.stash !== undefined
			);
			if (!other) return null;
			key = other[0];
		}
		const stash = sessions[key].stash ?? null;
		set((s) => {
			const { [key]: _taken, ...rest } = s.sessions;
			return { sessions: rest };
		});
		return stash;
	},
}));

/** True while a mounted editor for `path` is in Edit — in this pane when a
 *  pane id is given, in any pane otherwise. */
export function isEditingPath(
	sessions: Record<string, EditSession>,
	path: string,
	paneId?: string | null
): boolean {
	return Object.values(sessions).some(
		(s) => s.path === path && s.mounted && s.editing && (paneId == null || s.paneId === paneId)
	);
}

/** Session keys among `targets` that hold unsaved edits (mounted or stashed). */
export function dirtySessionKeys(
	targets: Array<{ path: string; paneId: string | null }>
): string[] {
	const { sessions } = useEditingStore.getState();
	const out: string[] = [];
	for (const t of targets) {
		const key = sessionKey(t.path, t.paneId);
		const s = sessions[key];
		if (s && !s.discarded && (s.dirty || s.stash !== undefined)) out.push(key);
	}
	return out;
}

export function anyDirtySession(): boolean {
	return Object.values(useEditingStore.getState().sessions).some(
		(s) => !s.discarded && (s.dirty || s.stash !== undefined)
	);
}

// Reload / window close with unsaved edits anywhere — mounted or stashed.
// A browser shows its own "leave site?" prompt; a Tauri window close does not
// honour beforeunload, so this is the browser half of the guard.
if (typeof window !== 'undefined') {
	window.addEventListener('beforeunload', (e) => {
		if (!anyDirtySession()) return;
		e.preventDefault();
		e.returnValue = '';
	});
}
