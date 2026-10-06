// Remote-session gates in tauri-cmd (2026-10-06 gap audit). Each helper must
// answer "not here" in a browser session served by ikenga-server, and keep
// the desktop answer otherwise.
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false, tauri: false }));

vi.mock('./transport', () => ({
	getTransport: () => ({ invoke: vi.fn(), listen: vi.fn() }),
	isRemoteWebSession: () => h.remote,
	isTauri: () => h.tauri,
}));

import { canOpenFilesWithOs } from './tauri-cmd';

afterEach(() => {
	h.remote = false;
	h.tauri = false;
});

describe('canOpenFilesWithOs (gap rank 14)', () => {
	it('is false in a remote browser session', () => {
		h.remote = true;
		expect(canOpenFilesWithOs()).toBe(false);
	});

	it('is true on the desktop', () => {
		h.tauri = true;
		expect(canOpenFilesWithOs()).toBe(true);
	});
});
