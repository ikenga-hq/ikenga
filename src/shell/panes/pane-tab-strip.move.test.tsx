// WP-07 §6A.2 / §6A.10: the tab context menu drops Split right / Split down
// (split now only reachable from the pane `⋯` menu, drag-to-edge, or
// ⌘\ / ⌘⇧\) and gains Move left / Move right — the single-pointer
// alternative to drag-reorder that WCAG 2.5.7 requires — plus
// "Move to new pane" right/down.

import { fireEvent, render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('./pane-views', () => ({
	viewLabel: (view: { kind: string; path?: string }) =>
		view.kind === 'route' ? (view.path ?? '/') : 'Terminal',
	viewSubtitle: () => '',
}));
vi.mock('@/terminal/use-terminal-titles', () => ({
	useTerminalTitles: () => undefined,
}));
vi.mock('./new-tab-menu', () => ({
	NewTabMenu: () => null,
	useAnchorRect: () => null,
}));

import { makeLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneView } from '@/lib/panes/types';
import { PaneTabStrip } from './pane-tab-strip';

const routeA: PaneView = { kind: 'route', path: '/a' };
const routeB: PaneView = { kind: 'route', path: '/b' };

function hydrateTwoTabs() {
	const leaf = makeLeaf(routeA);
	leaf.tabs = [routeA, routeB];
	usePaneStore.getState().hydrate({ root: leaf, focusedId: leaf.id, closedHistory: [] });
	return leaf;
}

beforeEach(() => {
	document.body.innerHTML = '';
});

describe('PaneTabStrip context menu (§6A.2, §6A.10)', () => {
	it('offers Move left/right and Move to new pane, and drops Split right/down', () => {
		const leaf = hydrateTwoTabs();
		render(<PaneTabStrip leaf={leaf} isFocused />);

		const tabs = screen.getAllByRole('tab');
		expect(tabs).toHaveLength(2);

		// Open the second tab's context menu (Move left should be enabled,
		// Move right disabled — it's already the last tab).
		fireEvent.contextMenu(tabs[1]);

		expect(screen.getByText('Move left')).toBeTruthy();
		expect(screen.getByText('Move right')).toBeTruthy();
		expect(screen.getByText('Move to new pane (right)')).toBeTruthy();
		expect(screen.getByText('Move to new pane (down)')).toBeTruthy();

		// Split is gone from this menu — it's ⋯ / drag / ⌘\ only now.
		expect(screen.queryByText('Split right')).toBeNull();
		expect(screen.queryByText('Split down')).toBeNull();

		// Move right is disabled on the last tab (nothing to its right).
		const moveRight = screen.getByText('Move right').closest('[role="menuitem"]');
		expect(moveRight?.getAttribute('data-disabled')).not.toBeNull();
	});

	it('reorders on "Move left"', () => {
		const leaf = hydrateTwoTabs();
		render(<PaneTabStrip leaf={leaf} isFocused />);

		fireEvent.contextMenu(screen.getAllByRole('tab')[1]);
		fireEvent.click(screen.getByText('Move left'));

		const after = usePaneStore.getState().root;
		if (after.type !== 'leaf') throw new Error('expected a leaf');
		expect(after.tabs.map((t) => (t.kind === 'route' ? t.path : t.kind))).toEqual(['/b', '/a']);
	});
});
