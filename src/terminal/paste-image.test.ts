import { beforeEach, describe, expect, it, vi } from 'vitest';
import { dismissToast, useToastStore } from '@/lib/toast';

const host = vi.hoisted(() => ({ browser: true }));
vi.mock('@/lib/transport', () => ({ isBrowserHost: () => host.browser }));

import {
	explainEmptyPaste,
	IMAGE_PASTE_UNSUPPORTED_MESSAGE,
	isImageOnlyPaste,
	onTerminalPasteEvent,
} from './paste-image';

function dt(opts: { text?: string; files?: number }): DataTransfer {
	return {
		files: { length: opts.files ?? 0 },
		items: Array.from({ length: opts.files ?? 0 }, () => ({ kind: 'file' })),
		getData: () => opts.text ?? '',
	} as unknown as DataTransfer;
}

function pasteEvent(data: DataTransfer) {
	return {
		clipboardData: data,
		preventDefault: vi.fn(),
		stopPropagation: vi.fn(),
	} as unknown as ClipboardEvent & { preventDefault: ReturnType<typeof vi.fn> };
}

beforeEach(() => {
	host.browser = true;
	while (useToastStore.getState().queue.length) dismissToast();
});

describe('terminal image paste', () => {
	it('detects an image/file paste with no text', () => {
		expect(isImageOnlyPaste(dt({ files: 1 }))).toBe(true);
		expect(isImageOnlyPaste(dt({ files: 1, text: 'caption' }))).toBe(false);
		expect(isImageOnlyPaste(dt({ text: 'hello' }))).toBe(false);
		expect(isImageOnlyPaste(null)).toBe(false);
	});

	it('toasts and swallows an image-only paste in a browser', () => {
		const e = pasteEvent(dt({ files: 1 }));
		onTerminalPasteEvent(e);
		expect(e.preventDefault).toHaveBeenCalled();
		expect(useToastStore.getState().queue.map((t) => t.label)).toEqual([
			IMAGE_PASTE_UNSUPPORTED_MESSAGE,
		]);
	});

	it('leaves text pastes alone', () => {
		const e = pasteEvent(dt({ text: 'ls' }));
		onTerminalPasteEvent(e);
		expect(e.preventDefault).not.toHaveBeenCalled();
		expect(useToastStore.getState().queue).toHaveLength(0);
	});

	it('does nothing on desktop', () => {
		host.browser = false;
		const e = pasteEvent(dt({ files: 1 }));
		onTerminalPasteEvent(e);
		expect(e.preventDefault).not.toHaveBeenCalled();
		expect(useToastStore.getState().queue).toHaveLength(0);
	});

	it('explains an empty keyboard paste when the clipboard holds an image', async () => {
		vi.stubGlobal('navigator', {
			clipboard: { read: async () => [{ types: ['image/png'] }] },
		});
		await explainEmptyPaste();
		expect(useToastStore.getState().queue).toHaveLength(1);
		vi.unstubAllGlobals();
	});

	it('stays quiet when the clipboard is just empty', async () => {
		vi.stubGlobal('navigator', {
			clipboard: { read: async () => [{ types: ['text/plain'] }] },
		});
		await explainEmptyPaste();
		expect(useToastStore.getState().queue).toHaveLength(0);
		vi.unstubAllGlobals();
	});
});
