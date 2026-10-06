// plans/file-editing Shape 4 — the explorer operations' rules, below the UI.

import { beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	remote: false,
	reqs: {} as Record<string, unknown>,
	fsKind: vi.fn(),
	fsWriteText: vi.fn(),
	fsMkdir: vi.fn(),
	fsRename: vi.fn(),
	fsTrash: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	fsKind: h.fsKind,
	fsWriteText: h.fsWriteText,
	fsMkdir: h.fsMkdir,
	fsRename: h.fsRename,
	fsTrash: h.fsTrash,
}));
vi.mock('@/lib/transport', () => ({ isRemoteWebSession: () => h.remote }));
vi.mock('@/lib/access/rpc-requirements.gen', () => ({ RPC_REQUIREMENTS: h.reqs }));

import { sessionKey, useEditingStore } from '@/lib/editing/editing-store';
import {
	SAVE_OR_DISCARD_FIRST,
	TRASH_UNAVAILABLE_MESSAGE,
	TRASH_UNAVAILABLE_REASON,
	createFile,
	createFolder,
	filesMenuDisabled,
	joinPath,
	moveEntry,
	nameError,
	renameEntry,
	resolveTarget,
	trashEntry,
	trashServed,
} from './file-ops';

beforeEach(() => {
	vi.clearAllMocks();
	h.remote = false;
	for (const k of Object.keys(h.reqs)) delete h.reqs[k];
	h.fsKind.mockResolvedValue('missing');
	h.fsWriteText.mockResolvedValue(undefined);
	h.fsMkdir.mockResolvedValue(undefined);
	h.fsTrash.mockResolvedValue(undefined);
	useEditingStore.setState({ sessions: {} });
});

describe('nameError', () => {
	it('accepts an ordinary name, including dotfiles', () => {
		expect(nameError('notes.md')).toBeNull();
		expect(nameError('  .env.example ')).toBeNull();
	});
	it('refuses empty, separators and . / ..', () => {
		expect(nameError('   ')).toMatch(/Enter a name/);
		expect(nameError('a/b')).toMatch(/can’t contain/);
		expect(nameError('a\\b')).toMatch(/can’t contain/);
		expect(nameError('.')).toMatch(/isn’t a valid name/);
		expect(nameError('..')).toMatch(/isn’t a valid name/);
	});
});

describe('joinPath', () => {
	it('joins with exactly one slash', () => {
		expect(joinPath('/r', 'a')).toBe('/r/a');
		expect(joinPath('/r/', 'a')).toBe('/r/a');
	});
});

describe('createFile', () => {
	it('writes an empty file at dir/name when the name is free', async () => {
		await expect(createFile('/r/src', ' a.ts ')).resolves.toBe('/r/src/a.ts');
		expect(h.fsKind).toHaveBeenCalledWith('/r/src/a.ts');
		expect(h.fsWriteText).toHaveBeenCalledWith('/r/src/a.ts', '');
	});

	it('refuses an existing name and writes nothing (fs_write would overwrite)', async () => {
		h.fsKind.mockResolvedValue('file');
		await expect(createFile('/r', 'a.ts')).rejects.toThrow('“a.ts” already exists here.');
		h.fsKind.mockResolvedValue('dir');
		await expect(createFile('/r', 'a.ts')).rejects.toThrow(/already exists/);
		expect(h.fsWriteText).not.toHaveBeenCalled();
	});

	it('refuses a bad name before touching the filesystem', async () => {
		await expect(createFile('/r', '../x')).rejects.toThrow(/can’t contain/);
		expect(h.fsKind).not.toHaveBeenCalled();
	});

	it('passes an allowlist refusal through as given', async () => {
		h.fsWriteText.mockRejectedValue(new Error('path not in allowlist: /etc/x'));
		await expect(createFile('/etc', 'x')).rejects.toThrow('path not in allowlist: /etc/x');
	});
});

describe('createFolder', () => {
	it('makes the folder when the name is free', async () => {
		await expect(createFolder('/r', 'lib')).resolves.toBe('/r/lib');
		expect(h.fsMkdir).toHaveBeenCalledWith('/r/lib');
	});

	it('refuses an existing folder (fs_mkdir would succeed silently)', async () => {
		h.fsKind.mockResolvedValue('dir');
		await expect(createFolder('/r', 'lib')).rejects.toThrow('“lib” already exists here.');
		expect(h.fsMkdir).not.toHaveBeenCalled();
	});
});

describe('renameEntry', () => {
	it('renames to a bare name', async () => {
		h.fsRename.mockResolvedValue('/r/b.ts');
		await expect(renameEntry('/r/a.ts', 'b.ts')).resolves.toBe('/r/b.ts');
		expect(h.fsRename).toHaveBeenCalledWith('/r/a.ts', 'b.ts');
	});

	it('turns the server collision error into a sentence', async () => {
		h.fsRename.mockRejectedValue(new Error('destination exists: /r/b.ts'));
		await expect(renameEntry('/r/a.ts', 'b.ts')).rejects.toThrow('“b.ts” already exists here.');
	});

	// Regression (F2): a path in the new name used to be refused outright,
	// so there was no way to move a file at all.
	it('a path in the new name moves the entry, relative to its folder', async () => {
		h.fsRename.mockResolvedValue('/r/newdir/renamed.txt');
		await expect(renameEntry('/r/a.txt', 'newdir/renamed.txt')).resolves.toBe(
			'/r/newdir/renamed.txt'
		);
		expect(h.fsRename).toHaveBeenCalledWith('/r/a.txt', 'renamed.txt', '/r/newdir');
		h.fsRename.mockResolvedValue('/x.txt');
		await renameEntry('/r/sub/a.txt', '../../x.txt');
		expect(h.fsRename).toHaveBeenLastCalledWith('/r/sub/a.txt', 'x.txt', '/');
	});

	it('a Move… path is relative to the given base; a leading / is absolute', async () => {
		h.fsRename.mockResolvedValue('/r/lib/a.ts');
		await renameEntry('/r/src/a.ts', 'lib/a.ts', '/r');
		expect(h.fsRename).toHaveBeenLastCalledWith('/r/src/a.ts', 'a.ts', '/r/lib');
		h.fsRename.mockResolvedValue('/elsewhere/a.ts');
		await renameEntry('/r/src/a.ts', '/elsewhere/a.ts', '/r');
		expect(h.fsRename).toHaveBeenLastCalledWith('/r/src/a.ts', 'a.ts', '/elsewhere');
	});

	it('refuses a bad last segment, a backslash or a trailing slash', async () => {
		for (const bad of ['sub/..', 'sub/', 'a\\b', 'sub/.']) {
			await expect(renameEntry('/r/a.ts', bad)).rejects.toThrow();
		}
		expect(h.fsRename).not.toHaveBeenCalled();
	});

	it('refuses while the file, or anything under a folder, has unsaved edits', async () => {
		useEditingStore.getState().upsert(sessionKey('/r/src/a.ts', 'p1'), {
			path: '/r/src/a.ts',
			paneId: 'p1',
			mounted: true,
			editing: true,
			dirty: true,
		});
		await expect(renameEntry('/r/src/a.ts', 'b.ts')).rejects.toThrow(SAVE_OR_DISCARD_FIRST);
		await expect(renameEntry('/r/src', 'lib')).rejects.toThrow(SAVE_OR_DISCARD_FIRST);
		// A sibling whose name only shares the prefix is not affected.
		h.fsRename.mockResolvedValue('/r/srcx2');
		await expect(renameEntry('/r/srcx', 'srcx2')).resolves.toBe('/r/srcx2');
		expect(h.fsRename).toHaveBeenCalledTimes(1);
	});

	it('allows a clean editor session', async () => {
		useEditingStore.getState().upsert(sessionKey('/r/a.ts', 'p1'), {
			path: '/r/a.ts',
			paneId: 'p1',
			mounted: true,
			editing: true,
			dirty: false,
		});
		h.fsRename.mockResolvedValue('/r/b.ts');
		await expect(renameEntry('/r/a.ts', 'b.ts')).resolves.toBe('/r/b.ts');
	});
});

describe('moveEntry', () => {
	it('moves into another folder, keeping the name', async () => {
		h.fsRename.mockResolvedValue('/r/newdir/a.ts');
		await expect(moveEntry('/r/a.ts', '/r/newdir')).resolves.toBe('/r/newdir/a.ts');
		expect(h.fsRename).toHaveBeenCalledWith('/r/a.ts', 'a.ts', '/r/newdir');
	});

	it('refuses to move a folder into itself or below itself', async () => {
		await expect(moveEntry('/r/src', '/r/src')).rejects.toThrow(/into itself/);
		await expect(moveEntry('/r/src', '/r/src/inner')).rejects.toThrow(/into itself/);
		expect(h.fsRename).not.toHaveBeenCalled();
	});

	it('the same folder and name is a no-op', async () => {
		await expect(moveEntry('/r/a.ts', '/r')).resolves.toBe('/r/a.ts');
		expect(h.fsRename).not.toHaveBeenCalled();
	});

	it('says plainly when the name is taken in the target folder', async () => {
		h.fsRename.mockRejectedValue(new Error('destination exists: /r/newdir/a.ts'));
		await expect(moveEntry('/r/a.ts', '/r/newdir')).rejects.toThrow(
			'“a.ts” already exists in newdir.'
		);
	});

	it('refuses while the entry has unsaved edits', async () => {
		useEditingStore.getState().upsert(sessionKey('/r/a.ts', 'p1'), {
			path: '/r/a.ts',
			paneId: 'p1',
			mounted: true,
			editing: true,
			dirty: true,
		});
		await expect(moveEntry('/r/a.ts', '/r/newdir')).rejects.toThrow(SAVE_OR_DISCARD_FIRST);
		expect(h.fsRename).not.toHaveBeenCalled();
	});

	it('a server that ignored the folder (renamed in place) is reported, not called a move', async () => {
		h.fsRename.mockResolvedValue('/r/a.ts');
		await expect(moveEntry('/r/a.ts', '/r/newdir')).rejects.toThrow(/can’t move files yet/);
	});
});

describe('resolveTarget', () => {
	it('folds . and .. and keeps a bare name in place', () => {
		expect(resolveTarget('/r/src', 'a.ts')).toEqual({ dir: '/r/src', name: 'a.ts' });
		expect(resolveTarget('/r/src', './x/../y/a.ts')).toEqual({ dir: '/r/src/y', name: 'a.ts' });
		expect(resolveTarget('/r', '/../a')).toHaveProperty('error');
	});
});

describe('Move to Trash — desktop and browser', () => {
	// Regression: trash did not check for unsaved edits, so an open draft was
	// left with no file behind it.
	it('refuses while the file, or anything under a folder, has unsaved edits', async () => {
		useEditingStore.getState().upsert(sessionKey('/r/src/plan.md', 'p1'), {
			path: '/r/src/plan.md',
			paneId: 'p1',
			mounted: false,
			dirty: true,
			stash: { draft: 'x', base: 'y', meta: { eol: '\n', bom: false } },
		});
		await expect(trashEntry('/r/src/plan.md')).rejects.toThrow(SAVE_OR_DISCARD_FIRST);
		await expect(trashEntry('/r/src')).rejects.toThrow(SAVE_OR_DISCARD_FIRST);
		expect(h.fsTrash).not.toHaveBeenCalled();
	});

	it('is served on the desktop', async () => {
		expect(trashServed()).toBe(true);
		expect(filesMenuDisabled('delete')).toBeUndefined();
		await trashEntry('/r/a.ts');
		expect(h.fsTrash).toHaveBeenCalledWith('/r/a.ts');
	});

	it('is not available in a browser whose daemon does not serve fs_trash', async () => {
		h.remote = true;
		expect(trashServed()).toBe(false);
		expect(filesMenuDisabled('delete')).toBe(TRASH_UNAVAILABLE_REASON);
		expect(filesMenuDisabled('rename')).toBeUndefined();
		await expect(trashEntry('/r/a.ts')).rejects.toThrow(TRASH_UNAVAILABLE_MESSAGE);
		expect(h.fsTrash).not.toHaveBeenCalled();
	});

	it('turns on in a browser once the served-verb table lists fs_trash', async () => {
		h.remote = true;
		h.reqs.fs_trash = { caps: ['files', 'dispatch'], class: 'shared' };
		expect(trashServed()).toBe(true);
		expect(filesMenuDisabled('delete')).toBeUndefined();
		await trashEntry('/r/a.ts');
		expect(h.fsTrash).toHaveBeenCalledWith('/r/a.ts');
	});

	it('maps an older daemon’s "not implemented" refusal to the plain sentence', async () => {
		h.remote = true;
		h.reqs.fs_trash = { caps: ['files', 'dispatch'], class: 'shared' };
		h.fsTrash.mockRejectedValue(new Error('fs_trash: not implemented in headless daemon'));
		await expect(trashEntry('/r/a.ts')).rejects.toThrow(TRASH_UNAVAILABLE_MESSAGE);
	});

	it('keeps any other error as given', async () => {
		h.fsTrash.mockRejectedValue(new Error('path not in allowlist: /etc/x'));
		await expect(trashEntry('/etc/x')).rejects.toThrow('path not in allowlist: /etc/x');
	});
});
