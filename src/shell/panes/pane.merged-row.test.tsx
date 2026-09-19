// WP-07 P2 (component-level slice): merged ↔ tabbed row switch at 1↔2 tabs
// (§6A.3), and the §6A.1 tools slot renders unconditionally (never
// conditionally unmounted — the reveal is purely `opacity`, which is what
// keeps a mid-reveal tooltip hoverable, WCAG 1.4.13). Layout/measurement
// assertions (bounding boxes, overflow, contrast, target sizes) need a real
// browser and live in `e2e/panes.spec.ts` per the WP-19 harness split
// (`e2e/README.md`).
//
// `PaneBody`, the terminal-title resolver and the new-tab menu are mocked out:
// they pull in the full route tree / react-query / xterm, none of which this
// slice needs — it only exercises the chrome around the active tab, not the
// tab's content.
//
// No `@testing-library/jest-dom` in this repo — plain DOM assertions only
// (`.toBeNull()`, `.textContent`, `.hasAttribute(...)`, etc.), matching every
// other component test here.

import { render, screen, within } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('./pane-views', () => ({
	PaneBody: () => null,
	useWebviewRoute: () => undefined,
	viewLabel: (view: { kind: string; path?: string }) =>
		view.kind === 'route' ? (view.path ?? '/') : 'Terminal',
	viewSubtitle: () => '',
}));
vi.mock('@/terminal/use-terminal-titles', () => ({
	useTerminalTitles: () => undefined,
}));
vi.mock('./pane-iyke-overlay', () => ({
	PaneIykeOverlay: () => null,
}));
vi.mock('./new-tab-menu', () => ({
	NewTabMenu: () => null,
	useAnchorRect: () => null,
}));

import { makeLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneView } from '@/lib/panes/types';
import { Pane } from './pane';

const routeView: PaneView = { kind: 'route', path: '/home' };
const terminalView: PaneView = { kind: 'terminal', sessionId: 'sess-1' };

function hydrateSingleLeaf(...tabs: PaneView[]) {
	const leaf = makeLeaf(tabs[0]);
	leaf.tabs = tabs;
	usePaneStore.getState().hydrate({ root: leaf, focusedId: leaf.id, closedHistory: [] });
	return leaf;
}

beforeEach(() => {
	document.body.innerHTML = '';
});

describe('Pane merged row (§6A.3)', () => {
	it('draws one row (no tablist) for a single address-bearing tab, with New tab + tools', () => {
		const leaf = hydrateSingleLeaf(routeView);
		render(<Pane leaf={leaf} />);

		// No tab strip — the merged row replaces it.
		expect(screen.queryByRole('tablist')).toBeNull();

		// The address input carries the address text; the row itself carries
		// the tab title as its aria-label per §6A.3.
		const address = screen.getByLabelText('Address') as HTMLInputElement;
		expect(address.value).toBe('/home');

		// "+ New tab" survives the merge (WP-19's frame smoke asserts this is
		// always reachable on the pane, regardless of tab count).
		expect(screen.getByRole('button', { name: 'New tab' })).toBeTruthy();

		// Tools slot: exactly the reserved `⟳ + ⋯` pair.
		expect(screen.getByRole('button', { name: 'Refresh pane' })).toBeTruthy();
		expect(screen.getByRole('button', { name: 'More pane actions' })).toBeTruthy();
	});

	it('draws the tab strip once a second tab exists', () => {
		const leaf = hydrateSingleLeaf(routeView, terminalView);
		render(<Pane leaf={leaf} />);

		const tablists = screen.getAllByRole('tablist');
		expect(tablists.length).toBeGreaterThan(0);
		expect(screen.getAllByRole('tab')).toHaveLength(2);
		expect(screen.getByRole('button', { name: 'New tab' })).toBeTruthy();
	});

	it('merges an address-less single tab (terminal) into one row too', () => {
		const leaf = hydrateSingleLeaf(terminalView);
		render(<Pane leaf={leaf} />);

		// Address-less kinds never had a second row (no address bar); the
		// tab strip itself is that one merged row, tools included.
		expect(screen.getAllByRole('tablist').length).toBeGreaterThan(0);
		expect(screen.getAllByRole('tab')).toHaveLength(1);
		expect(screen.getByRole('button', { name: 'Refresh pane' })).toBeTruthy();
	});

	it('never conditionally unmounts the pane-tools slot (§6A.1 opacity reveal, not display)', () => {
		// Two tabs, and the pane is deliberately NOT the focused one — the
		// assertion is purely structural: the tools exist in the DOM tree
		// whether or not this pane happens to be focused.
		const leaf = hydrateSingleLeaf(routeView, terminalView);
		usePaneStore.setState({ focusedId: 'some-other-pane-id' });
		render(<Pane leaf={leaf} />);

		const tools = document.querySelector('.pane-tools');
		expect(tools).not.toBeNull();
		// Opacity-based reveal only — never `hidden` or a Tailwind
		// display/visibility utility, which would also hide it from
		// assistive tech and drop it from the hit-test (1.4.13).
		expect(tools?.className).not.toMatch(/\bhidden\b/);
		expect(tools?.className).not.toMatch(/invisible/);
		expect(tools?.className).toMatch(/opacity-0/);
		const moreButton = within(tools as HTMLElement).getByRole('button', {
			name: 'More pane actions',
		});
		expect(moreButton.hasAttribute('disabled')).toBe(false);
	});
});
