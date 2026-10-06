// Explorer file operations — plans/file-editing Shape 4 (F2).
//
// New file, New folder, Rename and Move to Trash, all through the existing
// `tauri-cmd` fs wrappers. The allowlist is enforced on the other side
// (`resolve_allowlisted` on desktop, the daemon's PathGuard in a browser);
// this module only checks the name and turns the few errors a person can act
// on into plain sentences. Every other error is shown as given.
//
// Move is not here: `fs_rename` takes a bare basename, and there is no
// `fs_move` verb yet (a founder decision — see the plan's Shape 4).

import { create } from 'zustand';
import { RPC_REQUIREMENTS } from '@/lib/access/rpc-requirements.gen';
import { useEditingStore } from '@/lib/editing/editing-store';
import { fsKind, fsMkdir, fsRename, fsTrash, fsWriteText } from '@/lib/tauri-cmd';
import { isUnavailableOnServer } from '@/lib/transport/unavailable';
import { isRemoteWebSession } from '@/lib/transport';

export type CreateKind = 'file' | 'folder';

export const TRASH_UNAVAILABLE_REASON = 'Not available in the browser yet';
export const TRASH_UNAVAILABLE_MESSAGE = "Delete isn't available in the browser yet.";
export const SAVE_OR_DISCARD_FIRST = 'Save or discard changes first.';

/**
 * Why `name` can't be used as a file or folder name, or `null` when it can.
 * Mirrors `shared::fs::rename` (non-empty, no `/` or `\`) and adds `.`/`..`,
 * which would otherwise resolve to the folder itself or its parent.
 */
export function nameError(raw: string): string | null {
	const name = raw.trim();
	if (!name) return 'Enter a name.';
	if (name.includes('/') || name.includes('\\')) return 'A name can’t contain / or \\.';
	if (name === '.' || name === '..') return `“${name}” isn’t a valid name.`;
	return null;
}

export function joinPath(dir: string, name: string): string {
	return dir.endsWith('/') ? `${dir}${name}` : `${dir}/${name}`;
}

/**
 * Create an empty file `name` in `dir` and return its path. Refuses an
 * existing name first: `fs_write` overwrites without asking. `fs_kind` also
 * answers `'missing'` for a path outside the allowlist, so that case reaches
 * the write and fails there with the allowlist's own error.
 */
export async function createFile(dir: string, rawName: string): Promise<string> {
	const err = nameError(rawName);
	if (err) throw new Error(err);
	const name = rawName.trim();
	const target = joinPath(dir, name);
	if ((await fsKind(target)) !== 'missing') throw new Error(`“${name}” already exists here.`);
	await fsWriteText(target, '');
	return target;
}

/**
 * Create folder `name` in `dir` and return its path. `fs_mkdir` succeeds on a
 * folder that already exists, so the collision check happens here.
 */
export async function createFolder(dir: string, rawName: string): Promise<string> {
	const err = nameError(rawName);
	if (err) throw new Error(err);
	const name = rawName.trim();
	const target = joinPath(dir, name);
	if ((await fsKind(target)) !== 'missing') throw new Error(`“${name}” already exists here.`);
	await fsMkdir(target);
	return target;
}

/** True when an editor holds unsaved edits for `path` or (for a folder)
 *  anything under it — mounted or stashed. */
export function hasUnsavedEditsUnder(path: string): boolean {
	const prefix = path.endsWith('/') ? path : `${path}/`;
	return Object.values(useEditingStore.getState().sessions).some(
		(s) =>
			!s.discarded &&
			(s.dirty || s.stash !== undefined) &&
			(s.path === path || s.path.startsWith(prefix))
	);
}

/**
 * Rename `path` to the bare basename `rawName` in the same folder; returns
 * the new path. Refused while an editor holds unsaved edits for it: the
 * editor would keep saving to the old path.
 */
export async function renameEntry(path: string, rawName: string): Promise<string> {
	const err = nameError(rawName);
	if (err) throw new Error(err);
	if (hasUnsavedEditsUnder(path)) throw new Error(SAVE_OR_DISCARD_FIRST);
	const name = rawName.trim();
	try {
		return await fsRename(path, name);
	} catch (e) {
		const msg = e instanceof Error ? e.message : String(e);
		if (msg.includes('destination exists')) throw new Error(`“${name}” already exists here.`);
		throw e instanceof Error ? e : new Error(msg);
	}
}

/**
 * Whether Move to Trash can run here. The desktop always has it. A browser
 * has it once the daemon serves `fs_trash`; its row in the generated
 * served-verb table appears with that work package, and the SPA is served
 * from the same build, so this turns on by itself.
 *
 * Gates on `isRemoteWebSession()` only — a T1 browser session has no bearer
 * token, so a missing token must never read as "not remote".
 */
export function trashServed(): boolean {
	return !isRemoteWebSession() || 'fs_trash' in RPC_REQUIREMENTS;
}

/** The `files` menu's `disabled` callback. */
export function filesMenuDisabled(id: string): string | undefined {
	if (id === 'delete' && !trashServed()) return TRASH_UNAVAILABLE_REASON;
	return undefined;
}

/** Move `path` to the trash. A server that does not run `fs_trash` (an older
 *  daemon) gets a plain sentence instead of the raw RPC error. */
export async function trashEntry(path: string): Promise<void> {
	if (!trashServed()) throw new Error(TRASH_UNAVAILABLE_MESSAGE);
	try {
		await fsTrash(path);
	} catch (e) {
		const msg = e instanceof Error ? e.message : String(e);
		if (isRemoteWebSession() && isUnavailableOnServer(msg))
			throw new Error(TRASH_UNAVAILABLE_MESSAGE);
		throw e instanceof Error ? e : new Error(msg);
	}
}

/** The pending inline "create" row: one at a time, at the top of `dir`. */
interface PendingCreateState {
	pending: { dir: string; kind: CreateKind } | null;
	begin: (dir: string, kind: CreateKind) => void;
	cancel: () => void;
}

export const usePendingCreate = create<PendingCreateState>((set) => ({
	pending: null,
	begin: (dir, kind) => set({ pending: { dir, kind } }),
	cancel: () => set({ pending: null }),
}));
