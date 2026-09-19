// WP-09 T5 / T6 / T8 — the Shortcuts view lists every registry command grouped
// by region (against DEFAULT_KEYMAP), `?` and ⌘/ open it, it is keyboard
// operable, and the theme toggle is a palette action.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, renderHook, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	listAllSkillActions: () => Promise.resolve([]),
}));

import { DEFAULT_KEYMAP } from '@/lib/keymap/defaults';
import { labelFor } from '@/lib/keymap/registry';
import { useIkengaStore } from '@/lib/ikenga/theme-store';
import { CommandPalette, openCommandPalette, useCommandPalette } from './command-palette';
import { groupShortcuts, regionFor, SHORTCUT_REGIONS, ShortcutsView } from './shortcuts-view';

beforeAll(() => {
	// cmdk measures and scrolls its list; jsdom has neither API.
	class RO {
		observe() {}
		unobserve() {}
		disconnect() {}
	}
	(globalThis as { ResizeObserver?: unknown }).ResizeObserver ??= RO;
	Element.prototype.scrollIntoView ??= () => {};
});
afterEach(cleanup);

const COMMANDS = Array.from(new Set(DEFAULT_KEYMAP.map((e) => e.command)));

function fireKeydown(init: KeyboardEventInit, target: EventTarget = document.body) {
	const event = new KeyboardEvent('keydown', { ...init, cancelable: true, bubbles: true });
	target.dispatchEvent(event);
	return event;
}

describe('groupShortcuts() — T5', () => {
	it('covers every command in DEFAULT_KEYMAP exactly once', () => {
		const listed = groupShortcuts(DEFAULT_KEYMAP).flatMap((g) => g.rows.map((r) => r.command));
		expect(listed.slice().sort()).toEqual(COMMANDS.slice().sort());
		expect(new Set(listed).size).toBe(listed.length);
	});

	it('groups each command under its region, with no catch-all group', () => {
		const groups = groupShortcuts(DEFAULT_KEYMAP);
		expect(groups.find((g) => g.id === 'other')).toBeUndefined();
		for (const g of groups) {
			for (const r of g.rows) expect(regionFor(r.command).id).toBe(g.id);
		}
		// Rendered in the declared region order.
		const order = SHORTCUT_REGIONS.map((r) => r.id);
		const ids = groups.map((g) => g.id);
		expect(ids).toEqual(order.filter((id) => ids.includes(id)));
		expect(regionFor('rail.app').label).toBe('Rail');
		expect(regionFor('tab.close').id).toBe('panes');
		expect(regionFor('shortcuts.open').id).toBe('help');
	});

	it('takes every key label from labelFor()', () => {
		for (const mac of [true, false]) {
			for (const g of groupShortcuts(DEFAULT_KEYMAP, { mac })) {
				for (const r of g.rows) expect(r.keyLabel).toBe(labelFor(r.command, { mac }));
			}
		}
	});
});

describe('<ShortcutsView /> — T5 / T6', () => {
	it('renders every registry command under its region heading', () => {
		render(<ShortcutsView />);
		for (const g of groupShortcuts()) {
			const section = document.querySelector(`section[data-region="${g.id}"]`) as HTMLElement;
			expect(section, g.id).not.toBeNull();
			expect(within(section).getByRole('heading', { name: g.label })).toBeTruthy();
			for (const r of g.rows) {
				expect(section.querySelector(`[data-command="${r.command}"]`), r.command).not.toBeNull();
			}
		}
		const rows = document.querySelectorAll('[data-command]');
		expect(rows).toHaveLength(COMMANDS.length);
		// Reference-only rows: text, never inert buttons.
		expect(screen.queryAllByRole('button')).toHaveLength(0);
	});

	it('typing filters across all groups; filter and list are keyboard reachable', async () => {
		const user = userEvent.setup();
		render(<ShortcutsView onBack={() => {}} />);
		const input = screen.getByRole('textbox', { name: 'Filter keyboard shortcuts' });
		expect(document.activeElement).toBe(input);
		await user.type(input, 'split');
		const shown = Array.from(document.querySelectorAll<HTMLElement>('[data-command]')).map(
			(el) => el.dataset.command
		);
		expect(shown.sort()).toEqual(['pane.split-down', 'pane.split-right']);
		await user.tab();
		expect(document.activeElement).toBe(screen.getByRole('button', { name: 'All commands' }));
		await user.tab();
		expect(document.activeElement).toBe(screen.getByRole('region', { name: 'Keyboard shortcuts' }));
	});
});

describe('useCommandPalette() — `?` and ⌘/ open the Shortcuts view (T5)', () => {
	it('`?` opens shortcuts mode outside text fields, and closes it again', () => {
		const { result } = renderHook(() => useCommandPalette());
		act(() => {
			fireKeydown({ key: '?', shiftKey: true });
		});
		expect(result.current).toMatchObject({ open: true, mode: 'shortcuts' });
		act(() => {
			fireKeydown({ key: '?', shiftKey: true });
		});
		expect(result.current.open).toBe(false);
	});

	it('`?` types into a text field instead of opening', () => {
		const { result } = renderHook(() => useCommandPalette());
		const input = document.createElement('input');
		document.body.appendChild(input);
		act(() => {
			fireKeydown({ key: '?', shiftKey: true }, input);
		});
		expect(result.current.open).toBe(false);
		input.remove();
	});

	it('⌘/ (Ctrl+/ off macOS) opens shortcuts mode, and toggles from inside the palette', () => {
		const { result } = renderHook(() => useCommandPalette());
		// jsdom is not macOS, so `mod` resolves to Ctrl.
		expect(labelFor('shortcuts.open')).toBe('Ctrl+/');
		act(() => {
			fireKeydown({ key: '/', ctrlKey: true });
		});
		expect(result.current).toMatchObject({ open: true, mode: 'shortcuts' });
		const input = document.createElement('input');
		document.body.appendChild(input);
		act(() => {
			fireKeydown({ key: '/', ctrlKey: true }, input);
		});
		expect(result.current).toMatchObject({ open: true, mode: 'all' });
		input.remove();
	});

	it('openCommandPalette() lets frame chrome open any mode', () => {
		const { result } = renderHook(() => useCommandPalette());
		act(() => openCommandPalette('projects'));
		expect(result.current).toMatchObject({ open: true, mode: 'projects' });
		act(() => openCommandPalette('shortcuts'));
		expect(result.current).toMatchObject({ open: true, mode: 'shortcuts' });
	});
});

function renderPalette(mode: 'all' | 'shortcuts') {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={client}>
			<CommandPalette open mode={mode} onOpenChange={() => {}} />
		</QueryClientProvider>
	);
}

describe('<CommandPalette /> — T5 / T8', () => {
	it('mode `shortcuts` renders the grouped Shortcuts view in the palette dialog', () => {
		renderPalette('shortcuts');
		const dialog = screen.getByRole('dialog', { name: 'Keyboard shortcuts' });
		expect(dialog.querySelectorAll('[data-command]')).toHaveLength(COMMANDS.length);
	});

	it('T8: the theme toggle is a palette action (re-homed from the rail)', async () => {
		useIkengaStore.setState({ mode: 'dark' });
		const user = userEvent.setup();
		renderPalette('all');
		const row = screen.getByRole('option', { name: /Toggle theme: Dark → System/ });
		await user.click(row);
		expect(useIkengaStore.getState().mode).toBe('system');
		// Keyboard: filter to it and press Enter — keeps cycling.
		const input = screen.getByRole('combobox');
		await user.type(input, 'toggle theme');
		await user.keyboard('{Enter}');
		expect(useIkengaStore.getState().mode).toBe('light');
	});

	it('lists a "Keyboard shortcuts" palette row carrying the registry key hint', () => {
		renderPalette('all');
		const row = screen.getByRole('option', { name: /Keyboard shortcuts/ });
		expect(row.textContent).toContain(labelFor('shortcuts.open'));
	});
});
