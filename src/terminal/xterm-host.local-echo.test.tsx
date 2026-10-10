// XTermHost's local-echo wiring: attached in a browser session only, and on
// the desktop the keystroke path is exactly what it was — `term.onData` →
// `pty.write`, nothing in between. xterm and the engine are stand-ins; the
// component is the real one.

import { cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	browser: false,
	acked: true,
	onData: null as ((data: string) => void) | null,
	calls: [] as string[],
	handle: {
		onInput: vi.fn((data: string) => {
			h.calls.push(`predict:${data}`);
			return 7;
		}),
		ackInput: vi.fn(),
		setAltScreenProbe: vi.fn(),
		reset: vi.fn(),
		dispose: vi.fn(),
	},
	attach: vi.fn(),
}));

vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isBrowserSession: () => h.browser,
	isRemoteWebSession: () => h.browser,
}));

vi.mock('@/lib/transport/shims', () => ({
	readClipboardText: async () => '',
	writeClipboardText: async () => {},
}));

vi.mock('./local-echo/attach', () => ({
	attachLocalEcho: (...args: unknown[]) => {
		h.attach(...args);
		return h.handle;
	},
	allowAltScreenFor: () => false,
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
		focus() {}
		loadAddon() {}
		open() {}
		write() {}
		writeln() {}
		dispose() {}
		attachCustomKeyEventHandler() {}
		onData(fn: (data: string) => void) {
			h.onData = fn;
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
		mode: 'ephemeral' as const,
		cwd: '/',
		onData: () => () => {},
		onExit: () => () => {},
		write: vi.fn(async (data: string) => {
			h.calls.push(`write:${data}`);
		}),
		writeInput: vi.fn(async (data: string) => {
			h.calls.push(`write:${data}`);
			return h.acked;
		}),
		resize: async () => {},
		dispose: async () => {},
		primeExternalSnapshot: () => {},
	};
}

import { XTermHost } from './xterm-host';

beforeEach(() => {
	h.onData = null;
	h.calls = [];
	h.attach.mockClear();
	for (const fn of Object.values(h.handle)) fn.mockClear();
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

describe('XTermHost local echo', () => {
	it('desktop: never attached; keystrokes go straight to the PTY', async () => {
		h.browser = false;
		const pty = fakePty();
		render(<XTermHost pty={pty as any} disableWebgl />);
		expect(h.attach).not.toHaveBeenCalled();
		h.onData?.('a');
		expect(pty.write).toHaveBeenCalledWith('a');
		expect(h.calls).toEqual(['write:a']);
		expect(h.handle.onInput).not.toHaveBeenCalled();
	});

	it('browser: predicts before writing, then acks the write', async () => {
		h.browser = true;
		const pty = fakePty();
		const { unmount } = render(<XTermHost pty={pty as any} disableWebgl />);
		expect(h.attach).toHaveBeenCalledTimes(1);
		expect(h.handle.setAltScreenProbe).toHaveBeenCalledTimes(1);
		h.acked = true;
		h.onData?.('a');
		expect(h.calls).toEqual(['predict:a', 'write:a']);
		await waitFor(() => expect(h.handle.ackInput).toHaveBeenCalledWith(7, expect.any(Number)));
		unmount();
		expect(h.handle.dispose).toHaveBeenCalledTimes(1);
	});

	it('browser: a write only queued on the PTY socket is not an ack', async () => {
		h.browser = true;
		h.acked = false;
		const pty = fakePty();
		render(<XTermHost pty={pty as any} disableWebgl />);
		h.onData?.('b');
		await waitFor(() => expect(pty.writeInput).toHaveBeenCalledWith('b'));
		await Promise.resolve();
		expect(h.handle.ackInput).not.toHaveBeenCalled();
	});
});
