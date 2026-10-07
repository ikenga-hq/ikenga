import { afterEach, describe, expect, it } from 'vitest';

import { menuPasteBlockedHint, pasteKeyIsNative } from './paste-policy';

describe('paste-policy', () => {
	afterEach(() => {
		delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
	});

	it('leaves the paste keys to the browser in a browser session', () => {
		expect(pasteKeyIsNative()).toBe(true);
		expect(menuPasteBlockedHint()).toMatch(/Ctrl\+Shift\+V/);
	});

	it('keeps the programmatic Tauri clipboard path on the desktop', () => {
		(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
		expect(pasteKeyIsNative()).toBe(false);
		expect(menuPasteBlockedHint()).not.toMatch(/browser/);
	});
});
