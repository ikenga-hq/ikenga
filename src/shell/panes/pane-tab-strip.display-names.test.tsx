// Pane tab titles were showing title-cased pkg/ngwa-item ids
// ("Com.Ikenga.Studio") instead of the resolved display name ("Studio"):
// `viewLabel`'s `route` case returned the raw id, and this strip's CSS
// `capitalize` class (meant for ordinary route segments) treats `.` as a
// word boundary. This exercises the real `pane-views` + `use-pane-display-names`
// wiring end to end — only the resolver's data sources (pkgs, ngwa snapshot)
// are faked — so a regression in either the label text or the capitalize
// decision shows up here.

import { render, screen } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/terminal/use-terminal-titles', () => ({
	useTerminalTitles: () => undefined,
}));
vi.mock('./new-tab-menu', () => ({
	NewTabMenu: () => null,
	useAnchorRect: () => null,
}));
vi.mock('./use-pane-display-names', () => ({
	usePaneDisplayNameResolver: () => (id: string) =>
		id === 'com.ikenga.studio' ? 'Studio' : undefined,
}));

import { makeLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneView } from '@/lib/panes/types';
import { PaneTabStrip } from './pane-tab-strip';

const unresolvedPkgRoute: PaneView = { kind: 'route', path: '/pkg/com.ikenga.unknown-pkg' };
const resolvedPkgRoute: PaneView = { kind: 'route', path: '/pkg/com.ikenga.studio' };
const plainRoute: PaneView = { kind: 'route', path: '/settings/secrets' };

function hydrate(...tabs: PaneView[]) {
	const leaf = makeLeaf(tabs[0]);
	leaf.tabs = tabs;
	usePaneStore.getState().hydrate({ root: leaf, focusedId: leaf.id, closedHistory: [] });
	return leaf;
}

function labelSpan(text: string) {
	return screen.getByText(text);
}

beforeEach(() => {
	document.body.innerHTML = '';
});

describe('PaneTabStrip tab labels — pkg/ngwa display names (dotted ids never capitalize)', () => {
	it('shows the raw dotted id for an unresolved /pkg/<id> route, uncapitalized', () => {
		const leaf = hydrate(unresolvedPkgRoute, plainRoute);
		render(<PaneTabStrip leaf={leaf} isFocused />);

		const label = labelSpan('com.ikenga.unknown-pkg');
		expect(label.className).not.toMatch(/\bcapitalize\b/);
	});

	it('shows the resolved display name for a known /pkg/<id> route, uncapitalized', () => {
		const leaf = hydrate(resolvedPkgRoute, plainRoute);
		render(<PaneTabStrip leaf={leaf} isFocused />);

		const label = labelSpan('Studio');
		expect(label.className).not.toMatch(/\bcapitalize\b/);
	});

	it('still capitalizes an ordinary (non-pkg) route label', () => {
		const leaf = hydrate(plainRoute, resolvedPkgRoute);
		render(<PaneTabStrip leaf={leaf} isFocused />);

		const label = labelSpan('secrets');
		expect(label.className).toMatch(/\bcapitalize\b/);
	});
});
