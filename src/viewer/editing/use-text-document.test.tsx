// The save path's last line of defence (plans/file-editing F3, round-4
// review): Save checks the file against the base the draft was actually typed
// on, not merely the base on screen. So even a buffer change nobody foresaw —
// here the one quiet reload left, in View, followed at once by Edit and a
// keystroke computed on the text the editor showed before it — meets the
// conflict choice instead of overwriting the outside change.

import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useEditingStore } from '@/lib/editing/editing-store';
import { useTextDocument } from './use-text-document';

const h = vi.hoisted(() => ({
	disk: new Map<string, string>(),
	watchCb: null as null | ((change: { kind: string; path: string }) => unknown),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	fsRead: vi.fn(async (path: string) => {
		if (!h.disk.has(path)) throw new Error(`not found: ${path}`);
		return { bytes: Array.from(new TextEncoder().encode(h.disk.get(path))), mime: 'text/plain' };
	}),
	fsWriteText: vi.fn(async (path: string, text: string) => {
		h.disk.set(path, text);
	}),
	fsWatch: vi.fn(async () => 'w1'),
	fsListenWatch: vi.fn(async (_id: string, cb: (c: { kind: string; path: string }) => unknown) => {
		h.watchCb = cb;
		return () => {
			h.watchCb = null;
		};
	}),
	fsUnwatch: vi.fn(async () => {}),
}));

const tauri = await import('@/lib/tauri-cmd');
const fsWriteText = vi.mocked(tauri.fsWriteText);

beforeEach(() => {
	h.disk.clear();
	h.watchCb = null;
	vi.clearAllMocks();
	useEditingStore.setState({ sessions: {} });
});
afterEach(cleanup);

async function open(path: string, text: string) {
	h.disk.set(path, text);
	const hook = renderHook(() => useTextDocument({ path, paneId: 'p1' }));
	await waitFor(() => expect(hook.result.current.load.kind).toBe('ready'));
	await waitFor(() => expect(h.watchCb).not.toBeNull());
	return hook;
}

describe('useTextDocument — Save checks the base the draft was typed on', () => {
	it('an edit typed on the text from before a quiet reload never overwrites the change', async () => {
		const { result } = await open('/w/n.md', 'B line\n');
		const before = result.current;
		await act(async () => {
			// An agent writes; View takes it quietly…
			h.disk.set('/w/n.md', 'A agent wrote this\n');
			await h.watchCb?.({ kind: 'modify', path: '/w/n.md' });
			// …and before React renders it, the user is in Edit and a keystroke
			// arrives computed on the text the editor last showed.
			before.startEdit();
			before.setDraft('B line\nk');
		});
		expect(result.current.mode).toBe('edit');
		expect(result.current.base).toBe('A agent wrote this\n');
		expect(result.current.draft).toBe('B line\nk');

		await act(async () => {
			await result.current.save();
		});
		expect(fsWriteText).not.toHaveBeenCalled();
		expect(h.disk.get('/w/n.md')).toBe('A agent wrote this\n');
		expect(result.current.conflict).toEqual({ kind: 'changed', theirs: 'A agent wrote this\n' });

		// Keep mine is still the user's explicit choice.
		await act(async () => {
			await result.current.keepMine();
		});
		expect(h.disk.get('/w/n.md')).toBe('B line\nk');
	});

	it('typing after the editor rendered the new text saves normally', async () => {
		const { result } = await open('/w/m.md', 'B line\n');
		await act(async () => {
			h.disk.set('/w/m.md', 'A agent wrote this\n');
			await h.watchCb?.({ kind: 'modify', path: '/w/m.md' });
		});
		expect(result.current.draft).toBe('A agent wrote this\n');
		act(() => result.current.startEdit());
		act(() => result.current.setDraft('A agent wrote this\nk'));
		await act(async () => {
			await result.current.save();
		});
		expect(result.current.conflict).toBeNull();
		expect(h.disk.get('/w/m.md')).toBe('A agent wrote this\nk');
	});

	it('text typed while a save runs follows the write, so the next save is not a conflict', async () => {
		const { result } = await open('/w/s.md', 'one\n');
		act(() => result.current.startEdit());
		act(() => result.current.setDraft('one\ntwo\n'));
		// Hold the save's re-read so the user can type while it runs.
		let releaseRead: () => void = () => {};
		const realRead = vi.mocked(tauri.fsRead).getMockImplementation();
		vi.mocked(tauri.fsRead).mockImplementationOnce(async (p: string) => {
			await new Promise<void>((r) => {
				releaseRead = r;
			});
			return realRead ? realRead(p) : Promise.reject(new Error('no fsRead'));
		});
		let saving: Promise<void> = Promise.resolve();
		act(() => {
			saving = result.current.save();
		});
		await waitFor(() => expect(result.current.saveState.kind).toBe('saving'));
		act(() => result.current.setDraft('one\ntwo\nthree\n'));
		await act(async () => {
			releaseRead();
			await saving;
		});
		expect(h.disk.get('/w/s.md')).toBe('one\ntwo\n');
		await act(async () => {
			await result.current.save();
		});
		expect(result.current.conflict).toBeNull();
		expect(h.disk.get('/w/s.md')).toBe('one\ntwo\nthree\n');
	});
});
