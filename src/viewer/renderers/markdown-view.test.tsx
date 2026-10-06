// Markdown regression after moving its editor onto the shared editing surface
// (plans/file-editing): the read-only render is unchanged, and editing still
// has the split live preview, the formatting controls and save.

import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useEditingStore } from '@/lib/editing/editing-store';
import { MarkdownView } from './markdown-view';

const h = vi.hoisted(() => ({
	disk: new Map<string, string>(),
	fakeView: { hasFocus: true, state: { doc: { lines: 1 } } },
	watchCb: null as null | ((change: { kind: string; path: string }) => unknown),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	fsRead: vi.fn(async (path: string) => {
		if (!h.disk.has(path)) throw new Error('not found');
		return {
			bytes: Array.from(new TextEncoder().encode(h.disk.get(path)!)),
			mime: 'text/markdown',
		};
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

vi.mock('@/components/markdown', () => ({
	Markdown: ({ content, sourceLines }: { content: string; sourceLines?: boolean }) => (
		<div data-testid={sourceLines ? 'md-preview' : 'md'}>{content}</div>
	),
}));

// Scroll sync drives a real CodeMirror scrollDOM; out of scope here.
vi.mock('./use-scroll-sync', () => ({ useScrollSync: () => ({ onPreviewScroll: () => {} }) }));

vi.mock('./markdown-format', () => ({
	wrapSelection: vi.fn(),
	toggleLinePrefix: vi.fn(),
	insertLink: vi.fn(),
	formatMarkdown: vi.fn(async (s: string) => `${s.trim()}\n`),
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
			view: () => h.fakeView,
		}));
		return (
			<textarea
				aria-label={props.ariaLabel}
				value={props.value}
				onChange={(e) => props.onChange(e.target.value)}
			/>
		);
	});
	return { CodeEditor };
});

const tauri = await import('@/lib/tauri-cmd');
const fmt = await import('./markdown-format');

beforeEach(() => {
	h.disk.clear();
	h.watchCb = null;
	vi.clearAllMocks();
	useEditingStore.setState({ sessions: {} });
});
afterEach(cleanup);

describe('MarkdownView', () => {
	it('read-only: renders the document with no toolbar', async () => {
		h.disk.set('/d/a.md', '# Hello\n');
		render(<MarkdownView path="/d/a.md" />);
		expect((await screen.findByTestId('md')).textContent).toBe('# Hello\n');
		expect(screen.queryByRole('button', { name: 'Edit' })).toBeNull();
	});

	it('editable: preview first, then a source + live preview split', async () => {
		h.disk.set('/d/a.md', '# Hello\n');
		render(<MarkdownView path="/d/a.md" editable />);
		expect((await screen.findByTestId('md')).textContent).toBe('# Hello\n');
		fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
		const src = screen.getByLabelText('Markdown source') as HTMLTextAreaElement;
		fireEvent.change(src, { target: { value: '# Changed\n' } });
		expect(screen.getByTestId('md-preview').textContent).toBe('# Changed\n');
	});

	it('formatting controls act on the editor view', async () => {
		h.disk.set('/d/a.md', 'text\n');
		render(<MarkdownView path="/d/a.md" editable />);
		fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
		fireEvent.click(screen.getByRole('button', { name: 'Bold (⌘B)' }));
		expect(fmt.wrapSelection).toHaveBeenCalledWith(h.fakeView, '**', undefined);
		fireEvent.click(screen.getByRole('button', { name: 'Heading' }));
		expect(fmt.toggleLinePrefix).toHaveBeenCalledWith(h.fakeView, '## ');
		fireEvent.click(screen.getByRole('button', { name: 'Format document' }));
		await waitFor(() => expect(fmt.formatMarkdown).toHaveBeenCalled());
	});

	it('saves through the shared conditional save', async () => {
		h.disk.set('/d/a.md', 'one\n');
		render(<MarkdownView path="/d/a.md" editable />);
		fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
		fireEvent.change(screen.getByLabelText('Markdown source'), { target: { value: 'two\n' } });
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await waitFor(() => expect(tauri.fsWriteText).toHaveBeenCalledWith('/d/a.md', 'two\n'));
		// Still in Edit after saving (the editor is not remounted).
		expect(screen.getByLabelText('Markdown source')).toBeTruthy();
	});
});

// Regression (round-3 review, blocking 3): Format document awaited the
// formatter and then set the draft unconditionally. A clean buffer reloaded by
// the watcher meanwhile got the old text, formatted, put back over it — and
// since the base was now the new file, the next Save wrote it with no conflict,
// erasing the outside change. Format now runs as a buffer operation: the
// watcher's reload waits for it, and its result applies only to the text it
// formatted.
describe('MarkdownView — Format document and the file changing on disk', () => {
	const flush = () =>
		act(async () => {
			await new Promise((r) => setTimeout(r, 0));
		});

	function holdFormat() {
		let finish: (out: string) => void = () => {};
		vi.mocked(fmt.formatMarkdown).mockImplementationOnce(
			() =>
				new Promise<string>((r) => {
					finish = r;
				})
		);
		return (out: string) => finish(out);
	}

	async function openForEdit(path: string, text: string) {
		h.disk.set(path, text);
		render(<MarkdownView path={path} editable />);
		fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
		await waitFor(() => expect(h.watchCb).not.toBeNull());
		return screen.getByLabelText('Markdown source') as HTMLTextAreaElement;
	}

	it('an outside write during Format is never overwritten by the next Save', async () => {
		const src = await openForEdit('/d/n.md', '# Notes\n\nold line   \n');
		const finish = holdFormat();
		fireEvent.click(screen.getByRole('button', { name: 'Format document' }));
		await waitFor(() => expect(fmt.formatMarkdown).toHaveBeenCalledTimes(1));
		expect(
			(screen.getByRole('button', { name: 'Format document' }) as HTMLButtonElement).disabled
		).toBe(true);
		expect((screen.getByRole('button', { name: 'Cancel' }) as HTMLButtonElement).disabled).toBe(
			true
		);
		// An agent appends a line while the formatter runs.
		h.disk.set('/d/n.md', '# Notes\n\nold line   \nAGENT ADDED THIS\n');
		const reads = vi.mocked(tauri.fsRead).mock.calls.length;
		act(() => {
			void h.watchCb?.({ kind: 'modify', path: '/d/n.md' });
		});
		await flush();
		// The reload waits its turn: no read while the format is running.
		expect(vi.mocked(tauri.fsRead).mock.calls.length).toBe(reads);

		await act(async () => finish('# Notes\n\nold line\n'));
		await flush();
		// The formatted text is now unsaved edits on the old version, and the
		// reload that followed sees the file moved on: the conflict choice.
		await screen.findByText(/changed on disk since you opened it/);
		expect(src.value).toBe('# Notes\n\nold line\n');
		fireEvent.click(screen.getByRole('button', { name: 'Save' }));
		await flush();
		await screen.findByText(/changed on disk since you opened it/);
		expect(tauri.fsWriteText).not.toHaveBeenCalled();
		expect(h.disk.get('/d/n.md')).toBe('# Notes\n\nold line   \nAGENT ADDED THIS\n');
	});

	it('text typed while Format runs is kept, and the stale result is dropped with a note', async () => {
		const src = await openForEdit('/d/t.md', 'one   \n');
		const finish = holdFormat();
		fireEvent.click(screen.getByRole('button', { name: 'Format document' }));
		await waitFor(() => expect(fmt.formatMarkdown).toHaveBeenCalledTimes(1));
		fireEvent.change(src, { target: { value: 'one   \ntwo\n' } });
		await act(async () => finish('one\n'));
		await flush();
		expect(src.value).toBe('one   \ntwo\n');
		expect(screen.getByText(/Format not applied/)).toBeTruthy();
		expect(
			(screen.getByRole('button', { name: 'Format document' }) as HTMLButtonElement).disabled
		).toBe(false);
	});

	it('Format with nothing else going on still applies', async () => {
		const src = await openForEdit('/d/f.md', 'one   \n');
		fireEvent.click(screen.getByRole('button', { name: 'Format document' }));
		await waitFor(() => expect(src.value).toBe('one\n'));
		expect(screen.queryByText(/Format not applied/)).toBeNull();
	});
});
