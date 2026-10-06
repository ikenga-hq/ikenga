// Explorer file operations — plans/file-editing Shape 4 (F2).
//
// New file, New folder, Rename / Move and Move to Trash, all through the
// existing `tauri-cmd` fs wrappers. The allowlist is enforced on the other side
// (`resolve_allowlisted` on desktop, the daemon's PathGuard in a browser);
// this module only checks the name and turns the few errors a person can act
// on into plain sentences. Every other error is shown as given.
//
// A move is `fs_rename` with a destination folder (`toDir`), on both the
// desktop and the daemon. It is reached three ways: a rename whose new name
// holds a path ("newdir/renamed.txt"), the Move… item, and dragging a row onto
// a folder.

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

/** The folder holding `path` (no trailing slash). */
export function dirOf(path: string): string {
	const p = path.length > 1 ? path.replace(/\/+$/, '') : path;
	const idx = p.lastIndexOf('/');
	return idx > 0 ? p.slice(0, idx) : '/';
}

/** The last segment of `path`. */
export function baseName(path: string): string {
	const p = path.length > 1 ? path.replace(/\/+$/, '') : path;
	return p.slice(p.lastIndexOf('/') + 1);
}

/**
 * Where a rename / move input points: `raw` is a bare name, or a path —
 * relative to `fromDir`, or absolute when it starts with `/`. `.` and `..`
 * segments are folded here so the name check sees the real last segment; the
 * allowlist still has the final say on the other side.
 */
export function resolveTarget(
	fromDir: string,
	raw: string
): { dir: string; name: string } | { error: string } {
	const value = raw.trim();
	if (!value) return { error: 'Enter a name.' };
	if (value.includes('\\')) return { error: 'A name can’t contain \\.' };
	if (!value.includes('/')) {
		const err = nameError(value);
		return err ? { error: err } : { dir: fromDir, name: value };
	}
	const last = value.slice(value.lastIndexOf('/') + 1);
	if (!last || last === '.' || last === '..') {
		return { error: 'End the path with a file or folder name.' };
	}
	const start = value.startsWith('/') ? [] : fromDir.split('/').filter(Boolean);
	const parts = [...start];
	for (const seg of value.split('/')) {
		if (!seg || seg === '.') continue;
		if (seg === '..') {
			if (parts.length === 0) return { error: 'That path leaves the file system root.' };
			parts.pop();
		} else parts.push(seg);
	}
	const name = parts.pop();
	if (!name) return { error: 'Enter a name.' };
	const err = nameError(name);
	if (err) return { error: err };
	return { dir: `/${parts.join('/')}`, name };
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
 * Rename `path` to `raw`; returns the new path. A bare name renames in the
 * same folder. A path ("newdir/renamed.txt", "../x.txt", or absolute) is
 * resolved against `base` — the entry's own folder by default — and moves the
 * entry there (F2). Refused while an editor holds unsaved edits for it: the
 * editor would keep saving to the old path.
 */
export async function renameEntry(path: string, raw: string, base?: string): Promise<string> {
	const target = resolveTarget(base ?? dirOf(path), raw);
	if ('error' in target) throw new Error(target.error);
	return moveEntry(path, target.dir, target.name);
}

/**
 * Move `path` into the folder `destDir`, optionally under a new name; returns
 * the new path. A target in the entry's own folder is a plain rename.
 */
export async function moveEntry(path: string, destDir: string, rawName?: string): Promise<string> {
	const name = (rawName ?? baseName(path)).trim();
	const err = nameError(name);
	if (err) throw new Error(err);
	const from = path.length > 1 ? path.replace(/\/+$/, '') : path;
	const dir = destDir.length > 1 ? destDir.replace(/\/+$/, '') : destDir;
	if (dir === from || dir.startsWith(`${from}/`)) {
		throw new Error('A folder can’t be moved into itself.');
	}
	if (hasUnsavedEditsUnder(path)) throw new Error(SAVE_OR_DISCARD_FIRST);
	const sameFolder = dir === dirOf(from);
	if (sameFolder && name === baseName(from)) return from;
	try {
		const dest = sameFolder ? await fsRename(from, name) : await fsRename(from, name, dir);
		// A server that predates `toDir` would rename in place instead; say so
		// rather than report a move that did not happen.
		if (!sameFolder && dirOf(dest) === dirOf(from)) {
			throw new Error('This server can’t move files yet; the item was renamed in place.');
		}
		return dest;
	} catch (e) {
		const msg = e instanceof Error ? e.message : String(e);
		if (msg.includes('destination exists')) {
			throw new Error(
				sameFolder
					? `“${name}” already exists here.`
					: `“${name}” already exists in ${baseName(dir) || dir}.`
			);
		}
		if (msg.includes('not a folder')) throw new Error(`${baseName(dir) || dir} isn’t a folder.`);
		if (msg.includes('into itself')) throw new Error('A folder can’t be moved into itself.');
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
 *  daemon) gets a plain sentence instead of the raw RPC error. Refused while
 *  an editor holds unsaved edits for it or anything under it — those edits
 *  would be left with no file to save to. */
export async function trashEntry(path: string): Promise<void> {
	if (!trashServed()) throw new Error(TRASH_UNAVAILABLE_MESSAGE);
	if (hasUnsavedEditsUnder(path)) throw new Error(SAVE_OR_DISCARD_FIRST);
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
