// The rail (WP-03): inventory on a fresh and on a migrated v15 profile,
// keyboard reachability (roving tabindex), key bindings, pin Move up / Move
// down (WCAG 2.5.7), and the data-workspace hand-off that iframe pkgs observe.

import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ActivityPin } from '@/lib/tauri-cmd';

// Host-backed hooks the rail reads. Their own behaviour is tested elsewhere;
// here they are inert so the rail renders the same with or without Tauri.
vi.mock('@/lib/registry/use-updates-available', () => ({ useUpdatesAvailable: () => 0 }));
vi.mock('@/lib/pkg/use-activity-bar-entries', () => ({
	usePkgActivityBarEntries: () => ({ entries: [], loaded: true }),
}));

// Each test re-imports the store graph (vi.resetModules) so it rehydrates
// from the localStorage it seeded; a cold import of the frame is slow.
vi.setConfig({ testTimeout: 60_000 });

// Focusing a rail key opens its Radix tooltip, whose popper measures itself
// with ResizeObserver — absent from jsdom.
if (typeof globalThis.ResizeObserver === 'undefined') {
	globalThis.ResizeObserver = class {
		observe() {}
		unobserve() {}
		disconnect() {}
	} as unknown as typeof ResizeObserver;
}

type ActivityBarModule = typeof import('./activity-bar');
type ShellStoreModule = typeof import('@/lib/shell/shell-store');
type PinsStoreModule = typeof import('@/lib/shell/pins-store');
type ThemeStoreModule = typeof import('@/lib/ikenga/theme-store');
type PaneStoreModule = typeof import('@/lib/panes/pane-store');

interface Loaded {
	ActivityBar: ActivityBarModule['ActivityBar'];
	shell: ShellStoreModule;
	pins: PinsStoreModule;
	theme: ThemeStoreModule;
	panes: PaneStoreModule;
}

const SHELL_KEY = 'shell-store';

function pin(
	id: string,
	label: string,
	sortOrder: number,
	sectionId: string | null = null
): ActivityPin {
	return {
		id,
		kind: 'route',
		target: `/pkg/${id}`,
		label,
		iconLucide: null,
		iconEmoji: null,
		sectionId,
		sortOrder,
		createdAt: `2026-09-0${sortOrder + 1}T00:00:00Z`,
		manifestId: null,
		lastOpenedAt: null,
	};
}

const PINS = [pin('wiki', 'Wikipedia', 0), pin('sentry', 'Sentry', 1), pin('notion', 'Notion', 2)];

/** Fresh module graph so the stores rehydrate from whatever localStorage
 *  holds right now (the same technique as shell-store.test.ts). */
async function load(pins: ActivityPin[] = PINS): Promise<Loaded> {
	vi.resetModules();
	const shell = await import('@/lib/shell/shell-store');
	const pinsMod = await import('@/lib/shell/pins-store');
	const theme = await import('@/lib/ikenga/theme-store');
	const panes = await import('@/lib/panes/pane-store');
	const { ActivityBar } = await import('./activity-bar');
	const reorderPins = vi.fn(async (ids: string[], sectionId: string) => {
		// Mirror the store's optimistic reorder without the host round-trip.
		const cur = pinsMod.usePinsStore.getState().pins;
		const next = cur.map((p) => {
			const i = ids.indexOf(p.id);
			return i < 0 ? p : { ...p, sortOrder: i, sectionId: sectionId === '' ? null : sectionId };
		});
		pinsMod.usePinsStore.setState({ pins: next });
	});
	pinsMod.usePinsStore.setState({
		pins,
		sections: [],
		hydrated: true,
		hydrate: async () => {},
		reorderPins,
	});
	panes.usePaneStore.setState({ navigateFocused: vi.fn() } as never);
	return { ActivityBar, shell, pins: pinsMod, theme, panes };
}

function railItems(): string[] {
	return Array.from(document.querySelectorAll<HTMLElement>('[data-rail-item]')).map(
		(el) => el.dataset.railItem ?? ''
	);
}

/** Rail inventory with the foot's project switcher set aside — it is not a
 *  rail noun and moves to the title row in WP-09. */
function railNouns(): string[] {
	return railItems().filter((id) => id !== 'project-switcher');
}

function press(init: KeyboardEventInit, target: EventTarget = document.body) {
	const ev = new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...init });
	act(() => {
		target.dispatchEvent(ev);
	});
	return ev;
}

// ⌘ on mac, Ctrl elsewhere — jsdom's navigator is not mac, so `mod` = Ctrl.
const MOD = { ctrlKey: true };

beforeEach(() => {
	localStorage.clear();
	// No first-contact gloss in these tests (rail-gloss.test.tsx owns it).
	localStorage.setItem('ikenga.gloss.seen', JSON.stringify(['ngwa', 'chi']));
	vi.spyOn(console, 'warn').mockImplementation(() => {});
});

afterEach(() => {
	cleanup();
	vi.restoreAllMocks();
	localStorage.clear();
});

describe('rail inventory (R1)', () => {
	const EXPECTED = ['project', 'chi', 'ngwa', 'pin:wiki', 'pin:sentry', 'pin:notion', 'settings'];

	it('fresh profile: exactly Project · Chi · Ngwa · pins · Settings', async () => {
		const { ActivityBar, shell } = await load();
		render(<ActivityBar />);
		expect(railNouns()).toEqual(EXPECTED);
		expect(shell.useShellStore.getState().activeMode).toBe('project');
		expect(screen.getByRole('button', { name: 'Project' }).getAttribute('aria-current')).toBe(
			'page'
		);
		for (const name of ['Chi', 'Ngwa', 'Settings']) {
			expect(screen.getByRole('button', { name }).hasAttribute('aria-current')).toBe(false);
		}
		// Nothing from the pre-v16 rail survives.
		for (const gone of ['App', 'Files', 'Sessions', 'Artifact grid', 'Packages']) {
			expect(screen.queryByRole('button', { name: gone })).toBeNull();
		}
		expect(screen.queryByRole('button', { name: /^Theme:/ })).toBeNull();
		expect(screen.queryByRole('button', { name: /approvals? awaiting/ })).toBeNull();
	});

	it('migrated v15 profile (package mode active): the same inventory, Project active', async () => {
		localStorage.setItem(
			SHELL_KEY,
			JSON.stringify({
				state: {
					activeMode: 'pkg:com.ikenga.tasks',
					sidebarCollapsed: false,
					fileRoots: ['/home/ada/label'],
					claudeProjectRoots: [],
				},
				version: 15,
			})
		);
		const { ActivityBar, shell } = await load();
		render(<ActivityBar />);
		expect(shell.useShellStore.getState().activeMode).toBe('project');
		expect(railNouns()).toEqual(EXPECTED);
		expect(screen.getByRole('button', { name: 'Project' }).getAttribute('aria-current')).toBe(
			'page'
		);
	});

	it('migrated v15 profile on the old Packages mode lands on Ngwa', async () => {
		localStorage.setItem(SHELL_KEY, JSON.stringify({ state: { activeMode: 'pkgs' }, version: 15 }));
		const { ActivityBar } = await load([]);
		render(<ActivityBar />);
		expect(railNouns()).toEqual(['project', 'chi', 'ngwa', 'settings']);
		expect(screen.getByRole('button', { name: 'Ngwa' }).getAttribute('aria-current')).toBe('page');
	});
});

describe('keyboard (R2)', () => {
	it('the rail is one tab stop; ↑/↓/Home/End reach every item', async () => {
		const { ActivityBar } = await load();
		render(<ActivityBar />);
		const items = Array.from(document.querySelectorAll<HTMLElement>('[data-rail-item]'));
		// One tab stop: the active key.
		expect(items.filter((el) => el.tabIndex === 0).map((el) => el.dataset.railItem)).toEqual([
			'project',
		]);

		act(() => items[0]!.focus());
		const visited = [document.activeElement as HTMLElement];
		for (let i = 1; i < items.length; i++) {
			press({ key: 'ArrowDown' }, document.activeElement!);
			visited.push(document.activeElement as HTMLElement);
		}
		expect(visited).toEqual(items);
		// Wraps, and the tab stop follows focus.
		press({ key: 'ArrowDown' }, document.activeElement!);
		expect(document.activeElement).toBe(items[0]);
		press({ key: 'End' }, document.activeElement!);
		expect(document.activeElement).toBe(items[items.length - 1]);
		press({ key: 'ArrowUp' }, document.activeElement!);
		expect(document.activeElement).toBe(items[items.length - 2]);
		expect(items.filter((el) => el.tabIndex === 0)).toEqual([items[items.length - 2]]);
		press({ key: 'Home' }, document.activeElement!);
		expect(document.activeElement).toBe(items[0]);
	});

	it('every item is a native button carrying the focus-ring class', async () => {
		const { ActivityBar } = await load();
		render(<ActivityBar />);
		for (const el of document.querySelectorAll<HTMLElement>('[data-rail-item]')) {
			expect(el.tagName).toBe('BUTTON');
			expect(el.classList.contains('ikenga-rail-item')).toBe(true);
			expect(el.getAttribute('aria-label')).toBeTruthy();
		}
	});

	it('mod+1 / mod+2 / mod+3 / mod+, switch modes; mod+2 also focuses the Companion', async () => {
		const { ActivityBar, shell, panes } = await load();
		render(<ActivityBar />);
		const onFocusCompanion = vi.fn();
		window.addEventListener('ikenga:companion-focus', onFocusCompanion);

		press({ key: '2', ...MOD });
		expect(shell.useShellStore.getState().activeMode).toBe('chi');
		expect(onFocusCompanion).toHaveBeenCalledTimes(1);

		press({ key: '3', ...MOD });
		expect(shell.useShellStore.getState().activeMode).toBe('ngwa');
		expect(panes.usePaneStore.getState().navigateFocused).toHaveBeenLastCalledWith('/claude');

		press({ key: ',', ...MOD });
		expect(shell.useShellStore.getState().activeMode).toBe('settings');
		expect(panes.usePaneStore.getState().navigateFocused).toHaveBeenLastCalledWith(
			'/settings/appearance'
		);

		press({ key: '1', ...MOD });
		expect(shell.useShellStore.getState().activeMode).toBe('project');
		expect(onFocusCompanion).toHaveBeenCalledTimes(1);
		window.removeEventListener('ikenga:companion-focus', onFocusCompanion);
	});

	it('mod+4 / mod+5 / mod+6 are retired: unbound, no mode change', async () => {
		const { ActivityBar, shell } = await load();
		render(<ActivityBar />);
		for (const key of ['4', '5', '6']) {
			const ev = press({ key, ...MOD });
			expect(ev.defaultPrevented).toBe(false);
			expect(shell.useShellStore.getState().activeMode).toBe('project');
		}
	});

	it('clicking Chi focuses the Companion; clicking the active key toggles the sidebar', async () => {
		const { ActivityBar, shell } = await load();
		render(<ActivityBar />);
		const onFocusCompanion = vi.fn();
		window.addEventListener('ikenga:companion-focus', onFocusCompanion);
		fireEvent.click(screen.getByRole('button', { name: 'Chi' }));
		expect(shell.useShellStore.getState().activeMode).toBe('chi');
		expect(onFocusCompanion).toHaveBeenCalledTimes(1);

		const before = shell.useShellStore.getState().sidebarCollapsed;
		fireEvent.click(screen.getByRole('button', { name: 'Chi' }));
		expect(shell.useShellStore.getState().sidebarCollapsed).toBe(!before);
		expect(onFocusCompanion).toHaveBeenCalledTimes(1);
		window.removeEventListener('ikenga:companion-focus', onFocusCompanion);
	});
});

describe('pin menu — Move up / Move down (WCAG 2.5.7)', () => {
	async function openPinMenu(label: string) {
		fireEvent.contextMenu(screen.getByRole('button', { name: label }));
		return screen.findByRole('menu');
	}

	it('Move down swaps a pin with the next one, Move up with the previous one', async () => {
		const { ActivityBar, pins } = await load();
		render(<ActivityBar />);

		await openPinMenu('Wikipedia');
		fireEvent.click(await screen.findByRole('menuitem', { name: 'Move down' }));
		expect(pins.usePinsStore.getState().reorderPins).toHaveBeenLastCalledWith(
			['sentry', 'wiki', 'notion'],
			''
		);
		await waitFor(() =>
			expect(railNouns().filter((id) => id.startsWith('pin:'))).toEqual([
				'pin:sentry',
				'pin:wiki',
				'pin:notion',
			])
		);

		await openPinMenu('Notion');
		fireEvent.click(await screen.findByRole('menuitem', { name: 'Move up' }));
		expect(pins.usePinsStore.getState().reorderPins).toHaveBeenLastCalledWith(
			['sentry', 'notion', 'wiki'],
			''
		);
	});

	it('Move up is disabled on the first pin and Move down on the last', async () => {
		const { ActivityBar } = await load();
		render(<ActivityBar />);
		await openPinMenu('Wikipedia');
		expect(
			(await screen.findByRole('menuitem', { name: 'Move up' })).getAttribute('aria-disabled')
		).toBe('true');
		expect(screen.getByRole('menuitem', { name: 'Move down' }).hasAttribute('aria-disabled')).toBe(
			false
		);
		fireEvent.keyDown(screen.getByRole('menu'), { key: 'Escape' });
		await waitFor(() => expect(screen.queryByRole('menu')).toBeNull());

		await openPinMenu('Notion');
		expect(
			(await screen.findByRole('menuitem', { name: 'Move down' })).getAttribute('aria-disabled')
		).toBe('true');
	});
});

describe('data-workspace hand-off (R5)', () => {
	it('a rail click changes <html data-workspace>, which the iframe host observer sees', async () => {
		const { ActivityBar, theme } = await load();
		theme.installIkengaDomSync();
		render(<ActivityBar />);
		const html = document.documentElement;
		await waitFor(() => expect(html.getAttribute('data-workspace')).toBe('project'));

		// The exact observer `pkg-iframe-host.tsx` installs to re-push the
		// theme into every mounted iframe pkg.
		const repush = vi.fn();
		const observer = new MutationObserver(repush);
		observer.observe(html, {
			attributes: true,
			attributeFilter: ['data-mode', 'data-theme', 'data-tint-strength', 'data-workspace'],
		});

		fireEvent.click(screen.getByRole('button', { name: 'Ngwa' }));
		await waitFor(() => expect(repush).toHaveBeenCalled());
		expect(html.getAttribute('data-workspace')).toBe('ngwa');
		const records = repush.mock.calls.flatMap(([recs]) => recs as MutationRecord[]);
		expect(records.some((r) => r.attributeName === 'data-workspace')).toBe(true);
		observer.disconnect();
	});
});
