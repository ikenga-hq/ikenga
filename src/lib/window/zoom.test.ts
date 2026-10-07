import { afterEach, describe, expect, it, vi } from 'vitest';
import { hasCommandHandler, resetCommandTableForTests } from '@/lib/keymap/commands';
import { installZoom } from './zoom';

const session = vi.hoisted(() => ({ browser: false }));
vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isBrowserSession: () => session.browser,
	isTauri: () => !session.browser,
	listen: vi.fn(async () => () => {}),
	emit: vi.fn(async () => {}),
	getCurrentWebview: () => ({ setZoom: vi.fn(async () => {}) }),
}));

afterEach(() => {
	session.browser = false;
	resetCommandTableForTests();
});

describe('installZoom', () => {
	it('registers zoom.in / out / reset on the desktop', () => {
		const off = installZoom();
		for (const c of ['zoom.in', 'zoom.out', 'zoom.reset']) expect(hasCommandHandler(c)).toBe(true);
		off();
		expect(hasCommandHandler('zoom.in')).toBe(false);
	});

	it('registers no zoom command in a browser, so the browser keeps native zoom', () => {
		session.browser = true;
		const off = installZoom();
		for (const c of ['zoom.in', 'zoom.out', 'zoom.reset']) expect(hasCommandHandler(c)).toBe(false);
		expect(() => off()).not.toThrow();
	});

	it('a browser zoom keypress is not claimed (no preventDefault)', async () => {
		session.browser = true;
		installZoom();
		const { KeyDispatcher } = await import('@/lib/keymap/dispatcher');
		const d = new KeyDispatcher();
		const ev = new KeyboardEvent('keydown', { key: '=', ctrlKey: true, cancelable: true });
		d.handleKeydown(ev);
		expect(ev.defaultPrevented).toBe(false);
	});
});
