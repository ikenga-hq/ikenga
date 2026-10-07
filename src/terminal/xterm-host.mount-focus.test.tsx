// XTermHost's mount-time focus wiring: the host asks `shouldFocusTerminalOnMount`
// (the rule itself is covered in mount-focus.test.ts) with the right inputs and
// only calls `term.focus()` when it says yes. xterm, its addons and the PTY
// bridge are stand-ins; the component is the real one.

import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	web: true,
	focus: vi.fn(),
}));

vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isRemoteWebSession: () => h.web,
}));

vi.mock('@/lib/transport/shims', () => ({
	readClipboardText: async () => '',
	writeClipboardText: async () => {},
}));

vi.mock('@xterm/xterm', () => ({
	Terminal: class {
		rows = 24;
		cols = 80;
		options: Record<string, unknown> = {};
		unicode = { activeVersion: '6' };
		parser = { registerOscHandler: () => ({ dispose: () => {} }) };
		textarea = document.createElement('textarea');
		element = document.createElement('div');
		focus = h.focus;
		loadAddon() {}
		open() {}
		write() {}
		writeln() {}
		dispose() {}
		attachCustomKeyEventHandler() {}
		onData() {
			return { dispose: () => {} };
		}
		onResize() {
			return { dispose: () => {} };
		}
	},
}));
vi.mock('@xterm/addon-fit', () => ({
	FitAddon: class {
		fit() {}
	},
}));
vi.mock('@xterm/addon-unicode11', () => ({ Unicode11Addon: class {} }));
vi.mock('@xterm/addon-web-links', () => ({ WebLinksAddon: class {} }));
vi.mock('@xterm/addon-webgl', () => ({ WebglAddon: class {} }));
vi.mock('@xterm/addon-search', () => ({
	SearchAddon: class {
		dispose() {}
	},
}));
vi.mock('./path-links', () => ({ registerPathLinks: () => ({ dispose: () => {} }) }));
vi.mock('./osc133', () => ({
	setupSemanticPrompts: () => ({ dispose: () => {} }),
}));
vi.mock('./session-store', () => ({
	useTerminalStore: { subscribe: () => () => {}, getState: () => ({}) },
}));

function fakePty() {
	return {
		id: 'pty-0000000000',
		label: 'sh',
		mode: 'persistent' as const,
		cwd: '/',
		onData: () => () => {},
		onExit: () => () => {},
		write: async () => {},
		resize: async () => {},
		dispose: async () => {},
		primeExternalSnapshot: () => {},
	};
}

vi.mock('./pty-bridge', () => ({
	Pty: { spawn: vi.fn(async () => fakePty()) },
}));

import { XTermHost } from './xterm-host';

beforeEach(() => {
	h.focus.mockClear();
	vi.stubGlobal(
		'ResizeObserver',
		class {
			observe() {}
			disconnect() {}
			unobserve() {}
		}
	);
});
afterEach(() => {
	cleanup();
	vi.unstubAllGlobals();
});

// biome-ignore lint/suspicious/noExplicitAny: structural PTY stand-in
const pty = () => fakePty() as any;

describe('XTermHost mount focus (attach mode)', () => {
	it('remote web: a terminal in an unfocused pane does not take focus', () => {
		h.web = true;
		render(<XTermHost pty={pty()} focused={false} disableWebgl />);
		expect(h.focus).not.toHaveBeenCalled();
	});

	it('remote web: a terminal in the focused pane (or with no pane) takes focus', () => {
		h.web = true;
		render(<XTermHost pty={pty()} focused disableWebgl />);
		expect(h.focus).toHaveBeenCalledTimes(1);
		h.focus.mockClear();
		render(<XTermHost pty={pty()} disableWebgl />);
		expect(h.focus).toHaveBeenCalledTimes(1);
	});

	it('desktop: a fresh terminal always takes focus, as before', () => {
		h.web = false;
		render(<XTermHost pty={pty()} focused={false} disableWebgl />);
		expect(h.focus).toHaveBeenCalledTimes(1);
	});
});

describe('XTermHost mount focus (spawn mode, after the PTY comes up)', () => {
	const spec = { cmd: ['sh'], cwd: '/' };

	it('remote web: an unfocused pane stays unfocused after spawn', async () => {
		h.web = true;
		const { Pty } = await import('./pty-bridge');
		(Pty.spawn as ReturnType<typeof vi.fn>).mockClear();
		render(<XTermHost spec={spec} focused={false} disableWebgl />);
		await waitFor(() => expect(Pty.spawn).toHaveBeenCalled());
		await Promise.resolve();
		await Promise.resolve();
		expect(h.focus).not.toHaveBeenCalled();
	});

	it('remote web: the focused pane takes focus after spawn', async () => {
		h.web = true;
		render(<XTermHost spec={spec} focused disableWebgl />);
		await waitFor(() => expect(h.focus).toHaveBeenCalledTimes(1));
	});

	it('desktop: takes focus after spawn regardless of pane focus', async () => {
		h.web = false;
		render(<XTermHost spec={spec} focused={false} disableWebgl />);
		await waitFor(() => expect(h.focus).toHaveBeenCalledTimes(1));
	});
});
