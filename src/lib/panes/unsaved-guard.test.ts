import { beforeEach, describe, expect, it, vi } from 'vitest';
import {
	anyDirtySession,
	dirtySessionKeys,
	sessionKey,
	useEditingStore,
} from '@/lib/editing/editing-store';
import { findLeaf, makeLeaf } from './pane-reducer';
import { usePaneStore } from './pane-store';
import { guardedCloseActiveTab, guardedClosePane, guardedCloseTab } from './unsaved-guard';

const confirmMock = vi.hoisted(() => vi.fn(async (_message: string, _options?: unknown) => false));
vi.mock('@/lib/transport/dialog-shim', () => ({ confirm: confirmMock }));

function setup() {
	const root = makeLeaf({ kind: 'artifact', path: '/w/a.md' });
	usePaneStore.getState().hydrate({ root, focusedId: root.id, closedHistory: [] });
	usePaneStore.getState().addTab(root.id, { kind: 'artifact', path: '/w/b.md' });
	return root.id;
}

function tabs(paneId: string) {
	const r = usePaneStore.getState().root;
	return r.type === 'leaf' && r.id === paneId
		? r.tabs.map((t) => (t.kind === 'artifact' ? t.path : t.kind))
		: [];
}

beforeEach(() => {
	confirmMock.mockReset();
	useEditingStore.setState({ sessions: {} });
});

describe('unsaved-changes guard', () => {
	it('closes a clean tab without asking', async () => {
		const pane = setup();
		await guardedCloseTab(pane, 0);
		expect(confirmMock).not.toHaveBeenCalled();
		expect(tabs(pane)).toEqual(['/w/b.md']);
	});

	it('asks before closing a tab with unsaved edits; declining keeps it', async () => {
		const pane = setup();
		useEditingStore.getState().upsert(sessionKey('/w/a.md', pane), {
			path: '/w/a.md',
			paneId: pane,
			mounted: true,
			editing: true,
			dirty: true,
		});
		confirmMock.mockResolvedValueOnce(false);
		await guardedCloseTab(pane, 0);
		expect(confirmMock).toHaveBeenCalledTimes(1);
		expect(confirmMock.mock.calls[0]?.[0]).toMatch(/a\.md has unsaved changes/);
		expect(tabs(pane)).toEqual(['/w/a.md', '/w/b.md']);
	});

	it('confirming discards the session and closes the tab', async () => {
		const pane = setup();
		const key = sessionKey('/w/a.md', pane);
		useEditingStore
			.getState()
			.upsert(key, { path: '/w/a.md', paneId: pane, mounted: true, dirty: true });
		confirmMock.mockResolvedValueOnce(true);
		await guardedCloseTab(pane, 0);
		expect(tabs(pane)).toEqual(['/w/b.md']);
		expect(useEditingStore.getState().sessions[key]?.discarded).toBe(true);
	});

	it('a stashed draft (tab not active) also counts as unsaved', async () => {
		const pane = setup();
		// A second pane, so closing this one is a close the store allows.
		usePaneStore.getState().splitPane(pane, 'horizontal');
		useEditingStore.getState().upsert(sessionKey('/w/a.md', pane), {
			path: '/w/a.md',
			paneId: pane,
			mounted: false,
			dirty: true,
			stash: { draft: 'x', base: 'y', meta: { eol: '\n', bom: false } },
		});
		await guardedClosePane(pane);
		expect(confirmMock).toHaveBeenCalledTimes(1);
		expect(findLeaf(usePaneStore.getState().root, pane)?.tabs.length).toBe(2);
		// Confirming closes the pane, then discards the stash.
		confirmMock.mockResolvedValueOnce(true);
		await guardedClosePane(pane);
		expect(findLeaf(usePaneStore.getState().root, pane)).toBeNull();
		expect(dirtySessionKeys([{ path: '/w/a.md', paneId: pane }])).toEqual([]);
	});

	it('the active-tab close (⌘W) is guarded too', async () => {
		const pane = setup(); // b.md is active
		useEditingStore.getState().upsert(sessionKey('/w/b.md', pane), {
			path: '/w/b.md',
			paneId: pane,
			mounted: true,
			dirty: true,
		});
		await guardedCloseActiveTab();
		expect(confirmMock).toHaveBeenCalledTimes(1);
		expect(tabs(pane)).toEqual(['/w/a.md', '/w/b.md']);
	});

	// Regression: Discard used to mark the session discarded *before* the
	// close ran. The store refuses some closes; the session then stayed
	// "discarded" with live edits in it, invisible to this guard and to the
	// reload prompt, and a later tab switch dropped the draft without asking.
	describe('a close the store refuses discards nothing', () => {
		function single() {
			const root = makeLeaf({ kind: 'artifact', path: '/w/notes.md' });
			usePaneStore.getState().hydrate({ root, focusedId: root.id, closedHistory: [] });
			const key = sessionKey('/w/notes.md', root.id);
			useEditingStore.getState().upsert(key, {
				path: '/w/notes.md',
				paneId: root.id,
				mounted: true,
				editing: true,
				dirty: true,
			});
			return { pane: root.id, key };
		}

		function expectStillGuarded(pane: string, key: string) {
			expect(useEditingStore.getState().sessions[key]?.discarded).toBeFalsy();
			expect(dirtySessionKeys([{ path: '/w/notes.md', paneId: pane }])).toEqual([key]);
			expect(anyDirtySession()).toBe(true);
		}

		it('⌘W on the only tab of the only pane: no prompt, session intact', async () => {
			const { pane, key } = single();
			confirmMock.mockResolvedValue(true);
			await guardedCloseActiveTab();
			expect(confirmMock).not.toHaveBeenCalled();
			expect(tabs(pane)).toEqual(['/w/notes.md']);
			expectStillGuarded(pane, key);
		});

		it('closing the last pane: no prompt, session intact', async () => {
			const { pane, key } = single();
			confirmMock.mockResolvedValue(true);
			await guardedClosePane(pane);
			expect(confirmMock).not.toHaveBeenCalled();
			expectStillGuarded(pane, key);
		});

		it('a pinned tab (⌘W): no prompt, session intact', async () => {
			const pane = setup();
			const key = sessionKey('/w/b.md', pane);
			useEditingStore
				.getState()
				.upsert(key, { path: '/w/b.md', paneId: pane, mounted: true, dirty: true });
			usePaneStore.getState().toggleTabPinned(pane, 1);
			confirmMock.mockResolvedValue(true);
			await guardedCloseActiveTab();
			expect(confirmMock).not.toHaveBeenCalled();
			expect(tabs(pane)).toEqual(expect.arrayContaining(['/w/a.md', '/w/b.md']));
			expect(useEditingStore.getState().sessions[key]?.discarded).toBeFalsy();
			expect(anyDirtySession()).toBe(true);
		});
	});
});
