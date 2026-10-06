// Markdown regression after moving its editor onto the shared editing surface
// (plans/file-editing): the read-only render is unchanged, and editing still
// has the split live preview, the formatting controls and save.

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useEditingStore } from '@/lib/editing/editing-store';
import { MarkdownView } from './markdown-view';

const h = vi.hoisted(() => ({
	disk: new Map<string, string>(),
	fakeView: { hasFocus: true, state: { doc: { lines: 1 } } },
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
	fsListenWatch: vi.fn(async () => () => {}),
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
