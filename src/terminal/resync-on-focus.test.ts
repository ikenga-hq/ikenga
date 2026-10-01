import { describe, expect, it, vi } from 'vitest';
import { resyncPtySizeOnWindowFocus } from './resync-on-focus';

function fakeWindow() {
	const target = new EventTarget();
	return {
		addEventListener: target.addEventListener.bind(target),
		removeEventListener: target.removeEventListener.bind(target),
		focus: () => target.dispatchEvent(new Event('focus')),
	};
}

describe('resyncPtySizeOnWindowFocus', () => {
	it("pushes the terminal's current size to the PTY when the window regains focus", () => {
		const win = fakeWindow();
		const pty = { resize: vi.fn(async () => {}) };
		const term = { rows: 30, cols: 100 };
		resyncPtySizeOnWindowFocus(pty, term, win);

		win.focus();
		expect(pty.resize).toHaveBeenCalledTimes(1);
		expect(pty.resize).toHaveBeenCalledWith(30, 100);
	});

	it('reads the size at focus time, not at registration time', () => {
		const win = fakeWindow();
		const pty = { resize: vi.fn(async () => {}) };
		const term = { rows: 24, cols: 80 };
		resyncPtySizeOnWindowFocus(pty, term, win);

		term.rows = 40;
		term.cols = 132;
		win.focus();
		expect(pty.resize).toHaveBeenCalledWith(40, 132);
	});

	it('stops after dispose', () => {
		const win = fakeWindow();
		const pty = { resize: vi.fn(async () => {}) };
		const dispose = resyncPtySizeOnWindowFocus(pty, { rows: 24, cols: 80 }, win);

		dispose();
		win.focus();
		expect(pty.resize).not.toHaveBeenCalled();
	});

	it('swallows a rejected resize instead of raising an unhandled rejection', async () => {
		const win = fakeWindow();
		const pty = { resize: vi.fn(async () => Promise.reject(new Error('pty gone'))) };
		resyncPtySizeOnWindowFocus(pty, { rows: 24, cols: 80 }, win);

		expect(() => win.focus()).not.toThrow();
		await Promise.resolve();
	});

	it('is a no-op without a window (SSR / non-DOM test environments)', () => {
		const pty = { resize: vi.fn(async () => {}) };
		const dispose = resyncPtySizeOnWindowFocus(pty, { rows: 24, cols: 80 }, undefined);
		expect(dispose).toBeTypeOf('function');
		expect(() => dispose()).not.toThrow();
	});
});
