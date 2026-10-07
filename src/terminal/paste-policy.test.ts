import { afterEach, describe, expect, it } from 'vitest';

import { menuPasteBlockedHint, pasteKeyIsNative } from './paste-policy';

describe('paste-policy', () => {
	afterEach(() => {
		delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
	});

	it('leaves the paste keys to the browser in a browser session', () => {
		expect(pasteKeyIsNative()).toBe(true);
	});

	it('names the registry key per platform, not a hard-coded Ctrl+V', () => {
		expect(menuPasteBlockedHint({ mac: false })).toMatch(
			/press Ctrl\+Shift\+V \(or Ctrl\+V\) to paste/
		);
		const mac = menuPasteBlockedHint({ mac: true });
		expect(mac).toMatch(/⌘V/);
		expect(mac).not.toMatch(/Ctrl/);
	});

	it('keeps the programmatic Tauri clipboard path on the desktop', () => {
		(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {};
		expect(pasteKeyIsNative()).toBe(false);
		expect(menuPasteBlockedHint()).not.toMatch(/browser/);
	});
});
