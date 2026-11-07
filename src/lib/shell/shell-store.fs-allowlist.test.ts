// A project's extra roots reach the fs allowlist on the desktop app.
//
// WP-05 moved roots onto projects and dropped the `fileRoots` store, which was the only
// thing that wrote `fs_roots`. Every extra root a desktop user added was then refused by
// the explorer ("path outside allowlist"). A browser session must not widen its server's
// list this way: there the allowlist is the boundary.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const fsRootsAdd = vi.fn(async (path: string) => [path]);
const settingsWriteField = vi.fn(async () => null);

vi.mock('@/lib/tauri-cmd', async (importOriginal) => {
	const actual = await importOriginal<typeof import('@/lib/tauri-cmd')>();
	return {
		...actual,
		fsRootsAdd: (...a: unknown[]) => (fsRootsAdd as (...x: unknown[]) => unknown)(...a),
		settingsWriteField: (...a: unknown[]) =>
			(settingsWriteField as (...x: unknown[]) => unknown)(...a),
	};
});

async function freshStore() {
	vi.resetModules();
	const mod = await import('./shell-store');
	return mod.useShellStore;
}

const win = window as unknown as Record<string, unknown>;

describe('setProjectExtraRoots → fs allowlist', () => {
	beforeEach(() => {
		fsRootsAdd.mockClear();
		settingsWriteField.mockClear();
		sessionStorage.clear();
		localStorage.clear();
		delete win.__TAURI_INTERNALS__;
	});
	afterEach(() => {
		delete win.__TAURI_INTERNALS__;
	});

	it('allows each newly added root on the desktop app, once', async () => {
		win.__TAURI_INTERNALS__ = {};
		const store = await freshStore();
		store.getState().setProjectExtraRoots('default', ['C:\\work', ' C:\\media ']);
		await vi.waitFor(() => expect(fsRootsAdd).toHaveBeenCalledTimes(2));
		expect(fsRootsAdd.mock.calls.map((c) => c[0])).toEqual(['C:\\work', 'C:\\media']);

		// Re-saving the same list, or removing a root, asks for nothing more.
		fsRootsAdd.mockClear();
		store.getState().setProjectExtraRoots('default', ['C:\\work']);
		store.getState().setProjectExtraRoots('default', ['C:\\work', 'D:\\new']);
		await vi.waitFor(() => expect(fsRootsAdd).toHaveBeenCalledTimes(1));
		expect(fsRootsAdd).toHaveBeenCalledWith('D:\\new');
	});

	it('keeps the roots even when the allowlist refuses one', async () => {
		win.__TAURI_INTERNALS__ = {};
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
		fsRootsAdd.mockRejectedValueOnce(new Error('path is empty'));
		const store = await freshStore();
		store.getState().setProjectExtraRoots('default', ['/work']);
		await vi.waitFor(() => expect(warn).toHaveBeenCalled());
		expect(store.getState().projectExtraRoots.default).toEqual(['/work']);
		warn.mockRestore();
	});

	it("never touches a browser session's server list", async () => {
		sessionStorage.setItem('ikenga_auth_token', 'tok');
		const store = await freshStore();
		store.getState().setProjectExtraRoots('default', ['/srv/elsewhere']);
		await new Promise((r) => setTimeout(r, 30));
		expect(fsRootsAdd).not.toHaveBeenCalled();
	});
});
