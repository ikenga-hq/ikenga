// Gap audit rank 28 — the Artifact Studio loupe's DOM inspector calls
// `iyke_dom_query`, which only the desktop app's iyke viewer bridge answers.
// In a browser session the slot is `undefined`, so the right rail drops the
// DOM tab and the query is never sent; the desktop still probes.

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	remote: false,
	iykeDomQuery: vi.fn(async () => ({ text: '- document', generation: 1 })),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	isRemoteWebSession: () => h.remote,
	iykeDomQuery: h.iykeDomQuery,
	fsWatch: vi.fn(async () => 'w1'),
	fsUnwatch: vi.fn(async () => {}),
	fsListenWatch: vi.fn(async () => () => {}),
}));

import { domInspectorSlot } from './dom-inspector';
import { RightRail } from './right-rail';

function renderRail() {
	return render(
		<RightRail
			tab="dom"
			onChangeTab={() => {}}
			slots={{ terminal: <div>terminal</div>, dom: domInspectorSlot('L1', '/w/a.html') }}
		/>
	);
}

beforeEach(() => h.iykeDomQuery.mockClear());
afterEach(() => {
	cleanup();
	h.remote = false;
});

describe('loupe DOM inspector (gap rank 28)', () => {
	it('is hidden in a remote browser session and never calls iyke_dom_query', () => {
		h.remote = true;
		expect(domInspectorSlot('L1', '/w/a.html')).toBeUndefined();
		renderRail();
		expect(screen.queryByText('DOM')).toBeNull();
		expect(screen.getByText('terminal')).toBeTruthy();
		expect(h.iykeDomQuery).not.toHaveBeenCalled();
	});

	it('probes the iframe DOM on the desktop', async () => {
		renderRail();
		expect(screen.getByText('DOM')).toBeTruthy();
		await waitFor(() =>
			expect(h.iykeDomQuery).toHaveBeenCalledWith({ pane: 'L1', query: undefined })
		);
	});
});
