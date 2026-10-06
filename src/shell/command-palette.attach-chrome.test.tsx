// Gap audit rank 16 — "Attach Chrome profile / tab…" opens a picker that
// talks to managed Chrome through `iyke_endpoint`, which the headless daemon
// does not serve. The palette leaves the entry out in a remote browser
// session and keeps it on the desktop.

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false }));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => h.remote,
}));
vi.mock('./chrome-picker/chrome-picker-dialog', () => ({ ChromePickerDialog: () => null }));
vi.mock('./palette-actions', () => ({
	ActionsGroup: () => null,
	ChiGroup: () => null,
	ManageGroup: () => null,
}));
vi.mock('./shortcuts-view', () => ({ ShortcutsView: () => null }));
vi.mock('@/terminal/single-terminal', () => ({
	createClaudeTerminalSession: vi.fn(),
	createTerminalSession: vi.fn(),
}));

import { CommandPalette } from './command-palette';

// jsdom has no ResizeObserver / scrollIntoView; cmdk reaches for both.
globalThis.ResizeObserver ??= class {
	observe() {}
	unobserve() {}
	disconnect() {}
} as unknown as typeof ResizeObserver;
Element.prototype.scrollIntoView ??= () => {};

afterEach(() => {
	cleanup();
	h.remote = false;
});

describe('Attach Chrome palette entry (gap rank 16)', () => {
	it('is absent in a remote browser session', () => {
		h.remote = true;
		render(<CommandPalette open mode="all" onOpenChange={() => {}} />);
		expect(screen.queryByText('Attach Chrome profile / tab…')).toBeNull();
		// The rest of the group still renders.
		expect(screen.getByText('Pin focused route to activity bar…')).toBeTruthy();
	});

	it('is offered on the desktop', () => {
		render(<CommandPalette open mode="all" onOpenChange={() => {}} />);
		expect(screen.getByText('Attach Chrome profile / tab…')).toBeTruthy();
	});
});
