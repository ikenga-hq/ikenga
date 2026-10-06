// plans/file-editing Shape 4 — the Files explorer's New file, New folder,
// Rename and Move to Trash, driven through the tree as a person would, with
// the browser fs_trash fallback.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => {
	const shellState = {
		activeProject: { id: 'proj-1', root_path: '/root', extra_roots: [] as string[] },
		activeProjectId: 'proj-1',
	};
	const paneState = {
		focusedId: 'pane-1',
		addTab: (() => {}) as (...args: unknown[]) => void,
		placeView: () => true,
		revealRequest: null,
		navigateFocused: () => {},
	};
	return {
		remote: false,
		reqs: {} as Record<string, unknown>,
		shellState,
		paneState,
		listing: {} as Record<string, Array<{ name: string; path: string; isDir: boolean }>>,
	};
});

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	// The actions store watches its files once a context menu opens.
	listen: vi.fn(async () => () => {}),
	fsList: vi.fn(async (dir: string) => h.listing[dir] ?? []),
	fsSearch: vi.fn(),
	fsKind: vi.fn(),
	fsWriteText: vi.fn(),
	fsMkdir: vi.fn(),
	fsRename: vi.fn(),
	fsTrash: vi.fn(),
}));
vi.mock('@/lib/shell/shell-store', () => ({
	useShellStore: Object.assign((sel: (s: typeof h.shellState) => unknown) => sel(h.shellState), {
		getState: () => h.shellState,
		subscribe: () => () => {},
	}),
}));
vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: Object.assign((sel: (s: typeof h.paneState) => unknown) => sel(h.paneState), {
		getState: () => h.paneState,
	}),
}));
vi.mock('@/lib/layout-state', () => ({
	debounce: (fn: unknown) => fn,
	loadLayoutState: async (_k: string, d: unknown) => d,
	saveLayoutState: async () => {},
}));
vi.mock('@/lib/shell/use-git-status', () => ({ useGitStatus: () => ({ data: undefined }) }));
vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isRemoteWebSession: () => h.remote,
}));
vi.mock('@/lib/transport/dialog-shim', async (orig) => ({
	...(await orig<typeof import('@/lib/transport/dialog-shim')>()),
	confirm: vi.fn(async () => true),
}));
vi.mock('@/lib/access/rpc-requirements.gen', () => ({ RPC_REQUIREMENTS: h.reqs }));

import { sessionKey, useEditingStore } from '@/lib/editing/editing-store';
import { useFilesStore } from '@/lib/shell/files-store';
import * as cmd from '@/lib/tauri-cmd';
import { usePendingCreate } from './file-ops';
import { FILE_DRAG_MIME, FilesSection } from './files';

const fs = vi.mocked(cmd);

Element.prototype.scrollIntoView = function scrollIntoView() {};

global.ResizeObserver = class {
	observe() {}
	unobserve() {}
	disconnect() {}
};

function renderFiles() {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={qc}>
			<FilesSection projectId="proj-1" />
		</QueryClientProvider>
	);
}

/** The tree row for `path` (ListRow carries the path as its title). */
async function row(path: string) {
	return screen.findByTitle(path);
}

async function typeAndEnter(input: HTMLElement, value: string) {
	fireEvent.change(input, { target: { value } });
	fireEvent.keyDown(input, { key: 'Enter' });
}

beforeEach(() => {
	vi.clearAllMocks();
	h.remote = false;
	for (const k of Object.keys(h.reqs)) delete h.reqs[k];
	h.paneState.addTab = vi.fn();
	h.listing = {
		'/root': [
			{ name: 'src', path: '/root/src', isDir: true },
			{ name: 'package.json', path: '/root/package.json', isDir: false },
		],
		'/root/src': [{ name: 'index.ts', path: '/root/src/index.ts', isDir: false }],
	};
	fs.fsKind.mockResolvedValue('missing');
	fs.fsWriteText.mockResolvedValue(undefined);
	fs.fsMkdir.mockResolvedValue(undefined);
	fs.fsTrash.mockResolvedValue(undefined);
	useEditingStore.setState({ sessions: {} });
	usePendingCreate.setState({ pending: null });
	useFilesStore.setState({
		hydrated: true,
		expandedRoot: '/root',
		expanded: new Set(),
		selectedPath: null,
		queries: {},
		showHidden: false,
		showIgnored: false,
	});
});

afterEach(cleanup);

describe('New file / New folder', () => {
	it('root header New file creates an empty file, refreshes the listing and opens it', async () => {
		renderFiles();
		await row('/root/package.json');
		fireEvent.click(screen.getByLabelText('New file'));
		const input = await screen.findByLabelText('New file name');

		h.listing['/root'] = [
			...h.listing['/root'],
			{ name: 'notes.md', path: '/root/notes.md', isDir: false },
		];
		await typeAndEnter(input, 'notes.md');

		await waitFor(() => expect(fs.fsWriteText).toHaveBeenCalledWith('/root/notes.md', ''));
		expect(fs.fsKind).toHaveBeenCalledWith('/root/notes.md');
		expect(h.paneState.addTab).toHaveBeenCalledWith('pane-1', {
			kind: 'artifact',
			path: '/root/notes.md',
		});
		expect(await row('/root/notes.md')).toBeTruthy();
		expect(screen.queryByLabelText('New file name')).toBeNull();
		expect(useFilesStore.getState().selectedPath).toBe('/root/notes.md');
	});

	it('refuses a name that already exists and keeps the row open', async () => {
		fs.fsKind.mockResolvedValue('file');
		renderFiles();
		await row('/root/package.json');
		fireEvent.click(screen.getByLabelText('New file'));
		await typeAndEnter(await screen.findByLabelText('New file name'), 'package.json');

		expect((await screen.findByRole('alert')).textContent).toContain(
			'“package.json” already exists here.'
		);
		expect(fs.fsWriteText).not.toHaveBeenCalled();
		expect(screen.getByLabelText('New file name')).toBeTruthy();
	});

	it('refuses an invalid name before touching the filesystem', async () => {
		renderFiles();
		await row('/root/package.json');
		fireEvent.click(screen.getByLabelText('New folder'));
		await typeAndEnter(await screen.findByLabelText('New folder name'), 'a/b');

		expect((await screen.findByRole('alert')).textContent).toContain('can’t contain');
		expect(fs.fsKind).not.toHaveBeenCalled();
		expect(fs.fsMkdir).not.toHaveBeenCalled();
	});

	it('shows an allowlist refusal as given', async () => {
		fs.fsWriteText.mockRejectedValue(new Error('path not in allowlist: /root/x'));
		renderFiles();
		await row('/root/package.json');
		fireEvent.click(screen.getByLabelText('New file'));
		await typeAndEnter(await screen.findByLabelText('New file name'), 'x');
		expect((await screen.findByRole('alert')).textContent).toContain(
			'path not in allowlist: /root/x'
		);
	});

	it('Esc cancels, and leaving the row empty cancels', async () => {
		renderFiles();
		await row('/root/package.json');
		fireEvent.click(screen.getByLabelText('New file'));
		fireEvent.keyDown(await screen.findByLabelText('New file name'), { key: 'Escape' });
		expect(screen.queryByLabelText('New file name')).toBeNull();

		fireEvent.click(screen.getByLabelText('New file'));
		fireEvent.blur(await screen.findByLabelText('New file name'));
		await waitFor(() => expect(screen.queryByLabelText('New file name')).toBeNull());
		expect(fs.fsKind).not.toHaveBeenCalled();
	});

	it('New Folder on a collapsed folder row opens it and creates inside it', async () => {
		renderFiles();
		fireEvent.contextMenu(await row('/root/src'));
		fireEvent.click(await screen.findByRole('menuitem', { name: /New Folder/ }));

		const input = await screen.findByLabelText('New folder name');
		expect(useFilesStore.getState().expanded.has('/root/src')).toBe(true);
		await typeAndEnter(input, 'lib');
		await waitFor(() => expect(fs.fsMkdir).toHaveBeenCalledWith('/root/src/lib'));
		expect(fs.fsKind).toHaveBeenCalledWith('/root/src/lib');
		expect(h.paneState.addTab).not.toHaveBeenCalled();
	});

	it('New File on a file row creates in that file’s folder', async () => {
		useFilesStore.setState({ expanded: new Set(['/root/src']) });
		renderFiles();
		fireEvent.contextMenu(await row('/root/src/index.ts'));
		fireEvent.click(await screen.findByRole('menuitem', { name: /New File/ }));
		await typeAndEnter(await screen.findByLabelText('New file name'), 'util.ts');
		await waitFor(() => expect(fs.fsWriteText).toHaveBeenCalledWith('/root/src/util.ts', ''));
	});
});

describe('Rename', () => {
	it('renames through the row action and refreshes the parent listing', async () => {
		fs.fsRename.mockResolvedValue('/root/pkg.json');
		renderFiles();
		const r = await row('/root/package.json');
		fireEvent.click(within(r).getByLabelText('Rename'));
		const input = within(r).getByRole('textbox');

		h.listing['/root'] = [
			h.listing['/root'][0],
			{ name: 'pkg.json', path: '/root/pkg.json', isDir: false },
		];
		await typeAndEnter(input, 'pkg.json');
		await waitFor(() => expect(fs.fsRename).toHaveBeenCalledWith('/root/package.json', 'pkg.json'));
		expect(await row('/root/pkg.json')).toBeTruthy();
	});

	it('Rename… from the context menu keeps the input open', async () => {
		renderFiles();
		const r = await row('/root/package.json');
		fireEvent.contextMenu(r);
		fireEvent.click(await screen.findByRole('menuitem', { name: /Rename/ }));
		await new Promise((res) => setTimeout(res, 20));
		expect(within(r).getByRole('textbox')).toBeTruthy();
		expect(document.activeElement).toBe(within(r).getByRole('textbox'));
	});

	it('says plainly when the new name is taken', async () => {
		fs.fsRename.mockRejectedValue(new Error('destination exists: /root/src'));
		renderFiles();
		const r = await row('/root/package.json');
		fireEvent.click(within(r).getByLabelText('Rename'));
		await typeAndEnter(within(r).getByRole('textbox'), 'src');
		expect((await screen.findByRole('alert')).textContent).toContain('“src” already exists here.');
	});

	it('refuses while the file has unsaved edits', async () => {
		useEditingStore.getState().upsert(sessionKey('/root/package.json', 'pane-1'), {
			path: '/root/package.json',
			paneId: 'pane-1',
			mounted: true,
			editing: true,
			dirty: true,
		});
		renderFiles();
		const r = await row('/root/package.json');
		fireEvent.click(within(r).getByLabelText('Rename'));
		await typeAndEnter(within(r).getByRole('textbox'), 'pkg.json');
		expect((await screen.findByRole('alert')).textContent).toContain(
			'Save or discard changes first.'
		);
		expect(fs.fsRename).not.toHaveBeenCalled();
	});
});

// Regression (F2 "rename / move"): there was no way to move a file — a path
// in Rename was refused, dragging onto a folder did nothing, and the context
// menu had no Move item.
describe('Move', () => {
	function dataTransfer(path?: string) {
		const store: Record<string, string> = path ? { [FILE_DRAG_MIME]: path } : {};
		return {
			get types() {
				return Object.keys(store);
			},
			getData: (t: string) => store[t] ?? '',
			setData: (t: string, v: string) => {
				store[t] = v;
			},
			dropEffect: 'none',
			effectAllowed: 'all',
		};
	}

	it('a path in Rename moves the file into that folder', async () => {
		fs.fsRename.mockResolvedValue('/root/src/renamed.json');
		renderFiles();
		const r = await row('/root/package.json');
		fireEvent.click(within(r).getByLabelText('Rename'));
		await typeAndEnter(within(r).getByRole('textbox'), 'src/renamed.json');
		await waitFor(() =>
			expect(fs.fsRename).toHaveBeenCalledWith('/root/package.json', 'renamed.json', '/root/src')
		);
		expect(screen.queryByRole('alert')).toBeNull();
	});

	it('Move… in the context menu edits the path from the project folder', async () => {
		fs.fsRename.mockResolvedValue('/root/src/package.json');
		renderFiles();
		const r = await row('/root/package.json');
		fireEvent.contextMenu(r);
		fireEvent.click(await screen.findByRole('menuitem', { name: /Move…/ }));
		const input = (await within(r).findByRole('textbox')) as HTMLInputElement;
		expect(input.value).toBe('package.json');
		await typeAndEnter(input, 'src/package.json');
		await waitFor(() =>
			expect(fs.fsRename).toHaveBeenCalledWith('/root/package.json', 'package.json', '/root/src')
		);
	});

	it('dragging a file onto a folder moves it there', async () => {
		fs.fsRename.mockResolvedValue('/root/src/package.json');
		renderFiles();
		const file = await row('/root/package.json');
		const folder = await row('/root/src');
		const dt = dataTransfer();
		fireEvent.dragStart(file, { dataTransfer: dt });
		expect(dt.getData(FILE_DRAG_MIME)).toBe('/root/package.json');
		fireEvent.dragOver(folder, { dataTransfer: dt });
		fireEvent.drop(folder, { dataTransfer: dt });
		await waitFor(() =>
			expect(fs.fsRename).toHaveBeenCalledWith('/root/package.json', 'package.json', '/root/src')
		);
	});

	it('dropping a file back into its own folder does nothing', async () => {
		renderFiles();
		const file = await row('/root/package.json');
		fireEvent.drop(file, { dataTransfer: dataTransfer('/root/package.json') });
		await new Promise((res) => setTimeout(res, 20));
		expect(fs.fsRename).not.toHaveBeenCalled();
	});
});

describe('Move to Trash', () => {
	// Regression: trash did not check for unsaved edits.
	it('refuses while the file has unsaved edits, before asking', async () => {
		useEditingStore.getState().upsert(sessionKey('/root/package.json', 'pane-1'), {
			path: '/root/package.json',
			paneId: 'pane-1',
			mounted: true,
			editing: true,
			dirty: true,
		});
		renderFiles();
		const r = await row('/root/package.json');
		fireEvent.click(within(r).getByLabelText('Move to trash'));
		expect((await screen.findByRole('alert')).textContent).toContain(
			'Save or discard changes first.'
		);
		expect(fs.fsTrash).not.toHaveBeenCalled();
	});

	it('desktop: confirms, trashes and refreshes the listing', async () => {
		renderFiles();
		const r = await row('/root/package.json');
		h.listing['/root'] = [h.listing['/root'][0]];
		fireEvent.click(within(r).getByLabelText('Move to trash'));
		await waitFor(() => expect(fs.fsTrash).toHaveBeenCalledWith('/root/package.json'));
		await waitFor(() => expect(screen.queryByTitle('/root/package.json')).toBeNull());
	});

	it('browser without fs_trash: no trash action, and the menu item is disabled with the reason', async () => {
		h.remote = true;
		renderFiles();
		const r = await row('/root/package.json');
		expect(within(r).queryByLabelText('Move to trash')).toBeNull();
		// Rename still works there.
		expect(within(r).getByLabelText('Rename')).toBeTruthy();

		fireEvent.contextMenu(r);
		const item = await screen.findByRole('menuitem', { name: /Move to Trash/ });
		expect(item.getAttribute('aria-disabled')).toBe('true');
		expect(item.getAttribute('title')).toBe('Not available in the browser yet');
		fireEvent.click(item);
		expect(fs.fsTrash).not.toHaveBeenCalled();
	});

	it('browser with fs_trash served: uses it', async () => {
		h.remote = true;
		h.reqs.fs_trash = { caps: ['files', 'dispatch'], class: 'shared' };
		renderFiles();
		const r = await row('/root/package.json');
		fireEvent.click(within(r).getByLabelText('Move to trash'));
		await waitFor(() => expect(fs.fsTrash).toHaveBeenCalledWith('/root/package.json'));
	});

	it('browser: a daemon that turns out not to serve it gets the plain sentence, not the raw error', async () => {
		h.remote = true;
		h.reqs.fs_trash = { caps: ['files', 'dispatch'], class: 'shared' };
		fs.fsTrash.mockRejectedValue(
			new Error('HTTP RPC error: fs_trash not implemented in headless daemon')
		);
		renderFiles();
		const r = await row('/root/package.json');
		fireEvent.click(within(r).getByLabelText('Move to trash'));
		const alert = await screen.findByRole('alert');
		expect(alert.textContent).toContain("Delete isn't available in the browser yet.");
		expect(alert.textContent).not.toContain('headless');
	});
});
