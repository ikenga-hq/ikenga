// Behaviour of the shared editing surface (plans/file-editing Shape 1–3, 5):
// read-only until Edit, dirty → save writes once, Cancel restores, the F3
// conflict choice, validation blocking, the large-file guard, the watcher and
// ⌘S. The CodeMirror editor is replaced by a textarea; the fs wrappers by an
// in-memory disk.

import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { anyDirtySession, useEditingStore } from '@/lib/editing/editing-store';
import { isMacPlatform } from '@/lib/keymap/platform';
import { EditableTextFrame } from './editable-text-frame';
import { MAX_EDITABLE_BYTES } from './text-document';

const h = vi.hoisted(() => ({
	disk: new Map<string, Uint8Array>(),
	missing: new Set<string>(),
	watchCb: null as null | ((change: { kind: string; path: string }) => void),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	fsRead: vi.fn(async (path: string) => {
		if (h.missing.has(path) || !h.disk.has(path)) throw new Error(`not found: ${path}`);
		return { bytes: Array.from(h.disk.get(path)!), mime: 'text/plain' };
	}),
	fsWriteText: vi.fn(async (path: string, text: string) => {
		h.disk.set(path, new TextEncoder().encode(text));
		h.missing.delete(path);
	}),
	fsWatch: vi.fn(async () => 'w1'),
	fsListenWatch: vi.fn(async (_id: string, cb: (c: { kind: string; path: string }) => void) => {
		h.watchCb = cb;
		return () => {
			h.watchCb = null;
		};
	}),
	fsUnwatch: vi.fn(async () => {}),
}));

vi.mock('@ikenga/ui-lib', async () => {
	const React = await import('react');
	const CodeEditor = React.forwardRef(function CodeEditor(
		props: { value: string; onChange: (v: string) => void; ariaLabel?: string },
		ref: React.Ref<unknown>
	) {
		React.useImperativeHandle(ref, () => ({
			focus() {},
			insertAtCursor() {},
			getSelection: () => ({ from: 0, to: 0, text: '' }),
			view: () => null,
		}));
		return (
			<textarea
				aria-label={props.ariaLabel ?? 'editor'}
				value={props.value}
				onChange={(e) => props.onChange(e.target.value)}
			/>
		);
	});
	return { CodeEditor };
});

const tauri = await import('@/lib/tauri-cmd');
const fsWriteText = vi.mocked(tauri.fsWriteText);

const enc = (s: string) => new TextEncoder().encode(s);
const diskText = (p: string) => new TextDecoder().decode(h.disk.get(p));
const MOD = isMacPlatform() ? { metaKey: true } : { ctrlKey: true };

function mount(path: string, paneId = 'p1') {
	return render(
		<EditableTextFrame
			path={path}
			paneId={paneId}
			renderView={(text) => <pre data-testid="view">{text}</pre>}
		/>
	);
}

async function ready() {
	await screen.findByRole('button', { name: 'Edit' });
}

async function startEditing() {
	await ready();
	fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
	return screen.getByLabelText('File source') as HTMLTextAreaElement;
}

function type(el: HTMLTextAreaElement, value: string) {
	fireEvent.change(el, { target: { value } });
}

beforeEach(() => {
	h.disk.clear();
	h.missing.clear();
	h.watchCb = null;
	vi.clearAllMocks();
	useEditingStore.setState({ sessions: {} });
});
afterEach(cleanup);

describe('EditableTextFrame — edit, save, cancel', () => {
	it('is read-only until Edit is clicked (F4)', async () => {
		h.disk.set('/w/a.ts', enc('const a = 1;\n'));
		mount('/w/a.ts');
		await ready();
		expect(screen.getByTestId('view').textContent).toBe('const a = 1;\n');
		expect(screen.queryByLabelText('File source')).toBeNull();
		const ed = await startEditing();
		expect(ed.value).toBe('const a = 1;\n');
	});

	it('dirty → Save writes exactly once and clears the dirty mark', async () => {
		h.disk.set('/w/a.ts', enc('one\n'));
		mount('/w/a.ts');
		const ed = await startEditing();
		expect(screen.queryByLabelText('Unsaved changes')).toBeNull();
		type(ed, 'two\n');
		expect(screen.getByLabelText('Unsaved changes')).toBeTruthy();
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await waitFor(() => expect(fsWriteText).toHaveBeenCalledTimes(1));
		expect(fsWriteText).toHaveBeenCalledWith('/w/a.ts', 'two\n');
		await waitFor(() => expect(screen.queryByLabelText('Unsaved changes')).toBeNull());
		expect(diskText('/w/a.ts')).toBe('two\n');
	});

	it('Save does nothing when the buffer is clean', async () => {
		h.disk.set('/w/a.ts', enc('one\n'));
		mount('/w/a.ts');
		await startEditing();
		expect((screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled).toBe(true);
		expect(fsWriteText).not.toHaveBeenCalled();
	});

	it('Cancel restores the on-disk text and returns to view without writing', async () => {
		h.disk.set('/w/a.ts', enc('orig\n'));
		mount('/w/a.ts');
		const ed = await startEditing();
		type(ed, 'changed\n');
		fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
		expect(screen.getByTestId('view').textContent).toBe('orig\n');
		expect(fsWriteText).not.toHaveBeenCalled();
		const again = await startEditing();
		expect(again.value).toBe('orig\n');
	});

	it('Done is unavailable while there are unsaved changes', async () => {
		h.disk.set('/w/a.ts', enc('x'));
		mount('/w/a.ts');
		const ed = await startEditing();
		type(ed, 'y');
		expect((screen.getByRole('button', { name: 'Done' }) as HTMLButtonElement).disabled).toBe(true);
	});

	it('keeps CRLF line endings on save', async () => {
		h.disk.set('/w/a.txt', enc('a\r\nb\r\n'));
		mount('/w/a.txt');
		const ed = await startEditing();
		expect(ed.value).toBe('a\nb\n');
		type(ed, 'a\nb\nc\n');
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await waitFor(() => expect(fsWriteText).toHaveBeenCalledTimes(1));
		expect(diskText('/w/a.txt')).toBe('a\r\nb\r\nc\r\n');
	});

	it('⌘S / Ctrl+S saves while the editor has focus', async () => {
		h.disk.set('/w/a.ts', enc('one'));
		mount('/w/a.ts');
		const ed = await startEditing();
		type(ed, 'two');
		act(() => ed.focus());
		fireEvent.keyDown(ed, { key: 's', ...MOD });
		await waitFor(() => expect(fsWriteText).toHaveBeenCalledTimes(1));
		expect(diskText('/w/a.ts')).toBe('two');
	});
});

describe('EditableTextFrame — conditional save (F3)', () => {
	it('a file changed underneath shows the conflict choice and does not write', async () => {
		h.disk.set('/w/a.ts', enc('base\n'));
		mount('/w/a.ts');
		const ed = await startEditing();
		type(ed, 'mine\n');
		h.disk.set('/w/a.ts', enc('theirs\n'));
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await screen.findByText(/changed on disk since you opened it/);
		expect(fsWriteText).not.toHaveBeenCalled();
		expect(diskText('/w/a.ts')).toBe('theirs\n');
	});

	it('Keep mine overwrites only after the explicit choice', async () => {
		h.disk.set('/w/a.ts', enc('base\n'));
		mount('/w/a.ts');
		const ed = await startEditing();
		type(ed, 'mine\n');
		h.disk.set('/w/a.ts', enc('theirs\n'));
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await screen.findByText(/changed on disk/);
		expect(fsWriteText).not.toHaveBeenCalled();
		fireEvent.click(screen.getByRole('button', { name: /Keep mine/ }));
		await waitFor(() => expect(fsWriteText).toHaveBeenCalledTimes(1));
		expect(diskText('/w/a.ts')).toBe('mine\n');
		await waitFor(() => expect(screen.queryByText(/changed on disk/)).toBeNull());
	});

	it('Load theirs replaces the draft and writes nothing', async () => {
		h.disk.set('/w/a.ts', enc('base\n'));
		mount('/w/a.ts');
		const ed = await startEditing();
		type(ed, 'mine\n');
		h.disk.set('/w/a.ts', enc('theirs\n'));
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await screen.findByText(/changed on disk/);
		fireEvent.click(screen.getByRole('button', { name: /Load theirs/ }));
		expect((screen.getByLabelText('File source') as HTMLTextAreaElement).value).toBe('theirs\n');
		expect(screen.queryByLabelText('Unsaved changes')).toBeNull();
		expect(fsWriteText).not.toHaveBeenCalled();
	});

	it('Show diff renders theirs against mine', async () => {
		h.disk.set('/w/a.ts', enc('same\nold\n'));
		mount('/w/a.ts');
		const ed = await startEditing();
		type(ed, 'same\nmine\n');
		h.disk.set('/w/a.ts', enc('same\ntheirs\n'));
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await screen.findByText(/changed on disk/);
		fireEvent.click(screen.getByRole('button', { name: 'Show diff' }));
		await waitFor(() => expect(document.querySelector('[data-diff="mine"]')).not.toBeNull());
		expect(document.querySelector('[data-diff="mine"]')?.textContent).toContain('mine');
		expect(document.querySelector('[data-diff="theirs"]')?.textContent).toContain('theirs');
		expect(fsWriteText).not.toHaveBeenCalled();
	});

	it('a file deleted underneath shows the deleted choice and does not write', async () => {
		h.disk.set('/w/a.ts', enc('base'));
		mount('/w/a.ts');
		const ed = await startEditing();
		type(ed, 'mine');
		h.missing.add('/w/a.ts');
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await screen.findByText(/moved or deleted/);
		expect(fsWriteText).not.toHaveBeenCalled();
		fireEvent.click(screen.getByRole('button', { name: /Save anyway/ }));
		await waitFor(() => expect(fsWriteText).toHaveBeenCalledTimes(1));
		expect(diskText('/w/a.ts')).toBe('mine');
	});
});

describe('EditableTextFrame — watcher', () => {
	it('a change while dirty raises the conflict choice early', async () => {
		h.disk.set('/w/a.ts', enc('base'));
		mount('/w/a.ts');
		const ed = await startEditing();
		await waitFor(() => expect(h.watchCb).not.toBeNull());
		type(ed, 'mine');
		h.disk.set('/w/a.ts', enc('theirs'));
		await act(async () => h.watchCb?.({ kind: 'modify', path: '/w/a.ts' }));
		await screen.findByText(/changed on disk since you opened it/);
		expect(ed.value).toBe('mine');
	});

	it('a change while clean reloads quietly', async () => {
		h.disk.set('/w/a.ts', enc('base'));
		mount('/w/a.ts');
		const ed = await startEditing();
		await waitFor(() => expect(h.watchCb).not.toBeNull());
		h.disk.set('/w/a.ts', enc('theirs'));
		await act(async () => h.watchCb?.({ kind: 'modify', path: '/w/a.ts' }));
		await waitFor(() => expect(ed.value).toBe('theirs'));
		expect(screen.queryByText(/changed on disk since you opened it/)).toBeNull();
		expect(screen.getByText(/Reloaded/)).toBeTruthy();
	});

	it('ignores the event for our own write', async () => {
		h.disk.set('/w/a.ts', enc('base'));
		mount('/w/a.ts');
		const ed = await startEditing();
		await waitFor(() => expect(h.watchCb).not.toBeNull());
		type(ed, 'mine');
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await waitFor(() => expect(fsWriteText).toHaveBeenCalledTimes(1));
		type(ed, 'mine and more');
		await act(async () => h.watchCb?.({ kind: 'modify', path: '/w/a.ts' }));
		expect(screen.queryByText(/changed on disk/)).toBeNull();
		expect(ed.value).toBe('mine and more');
	});
});

describe('EditableTextFrame — validation (F1)', () => {
	it('invalid JSON blocks the save with the parse error', async () => {
		h.disk.set('/w/a.json', enc('{"a": 1}'));
		mount('/w/a.json');
		const ed = await startEditing();
		type(ed, '{"a": 1,}');
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await screen.findByText(/Invalid JSON/);
		expect(fsWriteText).not.toHaveBeenCalled();
		// Fixing it lets the save through.
		type(ed, '{"a": 2}');
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await waitFor(() => expect(fsWriteText).toHaveBeenCalledTimes(1));
		expect(screen.queryByText(/Invalid JSON/)).toBeNull();
	});

	it('invalid YAML blocks the save and offers Go to line', async () => {
		h.disk.set('/w/c.yaml', enc('a: 1\n'));
		mount('/w/c.yaml');
		const ed = await startEditing();
		type(ed, 'a: 1\nb: [1, 2\nc: 3\n');
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await screen.findByText(/Invalid YAML/);
		expect(screen.getByRole('button', { name: /Go to line/ })).toBeTruthy();
		expect(fsWriteText).not.toHaveBeenCalled();
	});

	it('invalid TOML blocks the save', async () => {
		h.disk.set('/w/c.toml', enc('a = 1\n'));
		mount('/w/c.toml');
		const ed = await startEditing();
		type(ed, 'a = 1\nb = [1,\nc');
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await screen.findByText(/Invalid TOML/);
		expect(fsWriteText).not.toHaveBeenCalled();
	});

	it('CSV saves as text with no check', async () => {
		h.disk.set('/w/t.csv', enc('a,b\n1,2\n'));
		mount('/w/t.csv');
		const ed = await startEditing();
		type(ed, 'a,b\n1,"unclosed\n');
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await waitFor(() => expect(fsWriteText).toHaveBeenCalledTimes(1));
	});
});

describe('EditableTextFrame — files that cannot be edited', () => {
	it('a file above the size limit opens read-only with the reason', async () => {
		h.disk.set('/w/big.txt', new Uint8Array(MAX_EDITABLE_BYTES + 10).fill(0x61));
		mount('/w/big.txt');
		await ready();
		const edit = screen.getByRole('button', { name: 'Edit' }) as HTMLButtonElement;
		expect(edit.disabled).toBe(true);
		expect(screen.getByText(/too large to edit here/)).toBeTruthy();
		fireEvent.click(edit);
		expect(screen.queryByLabelText('File source')).toBeNull();
		// A 2 MiB fixture through the number[] wire shape is slow under a
		// parallel run; the default 5s is not enough there.
	}, 30_000);

	it('a file that is not UTF-8 opens read-only', async () => {
		h.disk.set('/w/latin1.txt', new Uint8Array([0x63, 0x61, 0x66, 0xe9]));
		mount('/w/latin1.txt');
		await ready();
		expect((screen.getByRole('button', { name: 'Edit' }) as HTMLButtonElement).disabled).toBe(true);
		expect(screen.getByText(/not valid UTF-8/)).toBeTruthy();
	});
});

describe('EditableTextFrame — unsaved edits survive an unmount', () => {
	it('a tab switch (unmount) stashes the draft; remount restores it in Edit', async () => {
		h.disk.set('/w/a.ts', enc('base'));
		const first = mount('/w/a.ts');
		const ed = await startEditing();
		type(ed, 'mine');
		first.unmount();
		const s = useEditingStore.getState().sessions;
		expect(Object.values(s).some((v) => v.stash?.draft === 'mine')).toBe(true);

		mount('/w/a.ts');
		const back = (await screen.findByLabelText('File source')) as HTMLTextAreaElement;
		expect(back.value).toBe('mine');
		expect(screen.getByLabelText('Unsaved changes')).toBeTruthy();
	});

	it('registers an editing session the artifact view can read', async () => {
		h.disk.set('/w/a.ts', enc('base'));
		mount('/w/a.ts', 'pane-x');
		await startEditing();
		await waitFor(() => {
			const s = Object.values(useEditingStore.getState().sessions);
			expect(s).toContainEqual(
				expect.objectContaining({ path: '/w/a.ts', paneId: 'pane-x', mounted: true, editing: true })
			);
		});
	});
});

// Regression: a stashed draft was only taken back on the success path, so a
// file that had been moved, trashed or made uneditable while the tab was in
// the background showed "Couldn't read this file" (or a read-only view) while
// the draft sat unseen in the store — and the next unmount deleted it.
describe('EditableTextFrame — a stashed draft whose file changed shape', () => {
	async function stashDraft(path: string, draft: string) {
		const first = mount(path);
		const ed = await startEditing();
		type(ed, draft);
		first.unmount();
		expect(
			Object.values(useEditingStore.getState().sessions).some((v) => v.stash?.draft === draft)
		).toBe(true);
	}

	it('file gone: back into Edit with the draft and the "moved or deleted" choice', async () => {
		h.disk.set('/w/plan.md', enc('base'));
		await stashDraft('/w/plan.md', 'mine');
		h.disk.delete('/w/plan.md');
		mount('/w/plan.md');
		const ed = (await screen.findByLabelText('File source')) as HTMLTextAreaElement;
		expect(ed.value).toBe('mine');
		expect(screen.getByText(/moved or deleted/)).toBeTruthy();
		expect(screen.queryByText(/Couldn't read this file/)).toBeNull();
		fireEvent.click(screen.getByRole('button', { name: /Save anyway/ }));
		await waitFor(() => expect(fsWriteText).toHaveBeenCalledTimes(1));
		expect(diskText('/w/plan.md')).toBe('mine');
	});

	it('file gone, then switched away again: the draft is stashed again, not dropped', async () => {
		h.disk.set('/w/plan.md', enc('base'));
		await stashDraft('/w/plan.md', 'mine');
		h.disk.delete('/w/plan.md');
		const second = mount('/w/plan.md');
		await screen.findByLabelText('File source');
		second.unmount();
		expect(
			Object.values(useEditingStore.getState().sessions).some((v) => v.stash?.draft === 'mine')
		).toBe(true);
	});

	it('file no longer editable: read-only, says the edits are kept, and keeps them', async () => {
		h.disk.set('/w/notes.txt', enc('base'));
		await stashDraft('/w/notes.txt', 'mine');
		h.disk.set('/w/notes.txt', new Uint8Array([0x63, 0x61, 0x66, 0xe9]));
		const second = mount('/w/notes.txt');
		await ready();
		expect((screen.getByRole('button', { name: 'Edit' }) as HTMLButtonElement).disabled).toBe(true);
		expect(screen.getByText(/unsaved edits are kept/)).toBeTruthy();
		// Still counted as unsaved while shown…
		expect(anyDirtySession()).toBe(true);
		// …and still there after the next unmount.
		second.unmount();
		expect(
			Object.values(useEditingStore.getState().sessions).some((v) => v.stash?.draft === 'mine')
		).toBe(true);
	});
});

// Regression: our own last write (A1) was remembered and excused forever. Once
// the watcher adopted a newer disk text (A2) as the base, a revert of the file
// to A1 was ignored by the watcher and by the save check, so a draft built on
// A2 overwrote the revert without the conflict choice (F3).
describe('EditableTextFrame — a revert to our own earlier save is a conflict', () => {
	it('raises the conflict choice and does not write', async () => {
		h.disk.set('/w/a.ts', enc('A0'));
		mount('/w/a.ts');
		const ed = await startEditing();
		await waitFor(() => expect(h.watchCb).not.toBeNull());
		type(ed, 'A1');
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await waitFor(() => expect(diskText('/w/a.ts')).toBe('A1'));
		await waitFor(() => expect(screen.queryByLabelText('Unsaved changes')).toBeNull());
		// An agent writes A2; the clean buffer adopts it.
		h.disk.set('/w/a.ts', enc('A2'));
		await act(async () => h.watchCb?.({ kind: 'modify', path: '/w/a.ts' }));
		await waitFor(() => expect(ed.value).toBe('A2'));
		type(ed, 'D');
		// The agent reverts the file to A1.
		h.disk.set('/w/a.ts', enc('A1'));
		await act(async () => h.watchCb?.({ kind: 'modify', path: '/w/a.ts' }));
		await screen.findByText(/changed on disk since you opened it/);
		fsWriteText.mockClear();
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await screen.findByText(/changed on disk since you opened it/);
		expect(fsWriteText).not.toHaveBeenCalled();
		expect(diskText('/w/a.ts')).toBe('A1');
	});
});
