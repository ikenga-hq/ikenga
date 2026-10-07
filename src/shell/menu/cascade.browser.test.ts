import { beforeEach, describe, expect, it, vi } from 'vitest';
import { dismissToast, useToastStore } from '@/lib/toast';

const host = vi.hoisted(() => ({ browser: true }));

vi.mock('@/lib/transport', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/transport')>()),
	isBrowserHost: () => host.browser,
	isTauri: () => false,
}));

import { runPredefined, visibleMenuEntries } from './cascade';
import { MENU_TREE, resolveMenuTree } from './tree';

const labels = (entries: ReturnType<typeof visibleMenuEntries>) =>
	entries.flatMap((e) => (e.kind === 'separator' ? [] : [e.label]));

function menu(id: string) {
	const m = MENU_TREE.find((t) => t.id === id);
	if (!m) throw new Error(id);
	return m;
}

beforeEach(() => {
	host.browser = true;
	while (useToastStore.getState().queue.length) dismissToast();
});

describe('visibleMenuEntries', () => {
	it('hides Minimize / Maximize in a browser, keeps Fullscreen', () => {
		const out = visibleMenuEntries(resolveMenuTree(menu('window')), { browser: true });
		expect(labels(out)).toEqual(['Fullscreen']);
	});

	it('hides Quit in a browser and leaves no dangling separator', () => {
		const out = visibleMenuEntries(resolveMenuTree(menu('ikenga')), { browser: true });
		expect(labels(out)).not.toContain('Quit Ikenga');
		expect(out[out.length - 1]?.kind).not.toBe('separator');
	});

	it('keeps the window controls on desktop', () => {
		const out = visibleMenuEntries(resolveMenuTree(menu('window')), { browser: false });
		expect(labels(out)).toEqual(['Minimize', 'Maximize', 'Fullscreen']);
		const app = visibleMenuEntries(resolveMenuTree(menu('ikenga')), { browser: false });
		expect(labels(app)).toContain('Quit Ikenga');
	});

	it('keeps Paste visible in a browser (it explains itself)', () => {
		const out = visibleMenuEntries(resolveMenuTree(menu('edit')), { browser: true });
		expect(labels(out)).toContain('Paste');
	});
});

describe('runPredefined in a browser', () => {
	it('Paste shows a toast with the paste key instead of silently failing', async () => {
		const exec = vi.fn(() => false);
		(document as unknown as { execCommand: unknown }).execCommand = exec;
		await runPredefined('paste');
		expect(exec).not.toHaveBeenCalled();
		const [t] = useToastStore.getState().queue;
		expect(t?.label).toMatch(/Ctrl\+V|⌘V/);
	});

	it('Fullscreen requests fullscreen on the page', async () => {
		const request = vi.fn(async () => {});
		document.documentElement.requestFullscreen = request;
		Object.defineProperty(document, 'fullscreenElement', { value: null, configurable: true });
		await runPredefined('fullscreen');
		expect(request).toHaveBeenCalled();
	});

	it('Fullscreen leaves fullscreen when already in it', async () => {
		const exit = vi.fn(async () => {});
		document.exitFullscreen = exit;
		Object.defineProperty(document, 'fullscreenElement', {
			value: document.body,
			configurable: true,
		});
		await runPredefined('fullscreen');
		expect(exit).toHaveBeenCalled();
	});

	it('Fullscreen failure is reported', async () => {
		document.documentElement.requestFullscreen = vi.fn(async () => {
			throw new Error('denied');
		});
		Object.defineProperty(document, 'fullscreenElement', { value: null, configurable: true });
		await runPredefined('fullscreen');
		expect(useToastStore.getState().queue).toHaveLength(1);
	});

	it('Paste on desktop still uses execCommand', async () => {
		host.browser = false;
		const exec = vi.fn(() => true);
		(document as unknown as { execCommand: unknown }).execCommand = exec;
		await runPredefined('paste');
		expect(exec).toHaveBeenCalledWith('paste');
		expect(useToastStore.getState().queue).toHaveLength(0);
	});
});
