// Rail icons for Outbox approvals (R-1) and installed app pkgs (R-3).
//
// The Outbox key runs through the REAL `useOutboxApprovalsActive` (desktop
// branch: kernel status + draft list mocked at the edge) so the share-role rule
// is exercised end to end; the pkg entries come from a mutable fake of the
// kernel-snapshot hook.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { PkgActivityBarEntry } from '@/lib/pkg/use-activity-bar-entries';
import type { ActivityPin } from '@/lib/tauri-cmd';

const h = vi.hoisted(() => ({
	share: null as { role: string } | null,
	installed: [] as Array<{ id: string; enabled: boolean }>,
	drafts: [] as unknown[],
	pending: 0,
	bar: {
		entries: [] as unknown[],
		views: [] as unknown[],
		loaded: true,
		availablePkgIds: null as ReadonlySet<string> | null,
		enabledPkgIds: null as ReadonlySet<string> | null,
	},
	kv: new Map<string, string>(),
}));

vi.mock('@/shell/people/use-shared-projects', () => ({ useCurrentShare: () => h.share }));
vi.mock('@/lib/queries/pa-actions', () => ({
	paActionsListQueryOptions: () => ({
		queryKey: ['pa-actions-test'],
		queryFn: async () => h.drafts,
	}),
}));
vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isBrowserSession: () => false,
}));
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	pkgKernelStatus: async () => ({ installed: h.installed, registries: {}, api_version: 1 }),
	settingsGet: async (k: string) => h.kv.get(k) ?? null,
	settingsSet: async (k: string, v: string) => {
		h.kv.set(k, v);
	},
}));
vi.mock('@/shell/status-bar', () => ({
	APPROVALS_ROUTE: '/outbox/approvals',
	usePendingApprovalsCount: () => h.pending,
}));
vi.mock('@/lib/registry/use-updates-available', () => ({ useUpdatesAvailable: () => 0 }));
vi.mock('@/lib/pkg/use-activity-bar-entries', () => ({
	usePkgActivityBarEntries: () => h.bar,
}));

if (typeof globalThis.ResizeObserver === 'undefined') {
	globalThis.ResizeObserver = class {
		observe() {}
		unobserve() {}
		disconnect() {}
	} as unknown as typeof ResizeObserver;
}
vi.setConfig({ testTimeout: 60_000 });

import { usePaneStore } from '@/lib/panes/pane-store';
import { usePinsStore } from '@/lib/shell/pins-store';
import { RAIL_HIDDEN_KV, useRailAutoStore } from '@/lib/shell/rail-auto-icons';
import { ActivityBar } from './activity-bar';

function entry(id: string, name = id): PkgActivityBarEntry {
	return {
		pkg_id: id,
		pkg_name: name,
		id: 'main',
		label: name,
		icon: null,
		route: `/pkg/${id}/`,
		nav: [],
	};
}

function pin(id: string, target: string, manifestId: string | null = null): ActivityPin {
	return {
		id,
		kind: 'route',
		target,
		label: id,
		iconLucide: null,
		iconEmoji: null,
		sectionId: null,
		sortOrder: 0,
		createdAt: '2026-09-01T00:00:00Z',
		manifestId,
		lastOpenedAt: null,
	};
}

function setPkgs(pkgs: Array<{ id: string; enabled?: boolean }>) {
	h.bar.entries = pkgs.map((p) => entry(p.id, p.id.toUpperCase()));
	h.bar.enabledPkgIds = new Set(pkgs.filter((p) => p.enabled !== false).map((p) => p.id));
	h.bar.availablePkgIds = new Set(pkgs.map((p) => p.id));
}

function mount() {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={qc}>
			<ActivityBar />
		</QueryClientProvider>
	);
}

const items = () =>
	Array.from(document.querySelectorAll<HTMLElement>('[data-rail-item]')).map(
		(el) => el.dataset.railItem ?? ''
	);

const navigateFocused = vi.fn();

beforeEach(() => {
	localStorage.clear();
	localStorage.setItem('ikenga.gloss.seen', JSON.stringify(['ngwa', 'chi']));
	vi.spyOn(console, 'warn').mockImplementation(() => {});
	h.share = null;
	h.installed = [];
	h.drafts = [];
	h.pending = 0;
	h.kv.clear();
	h.bar.entries = [];
	h.bar.enabledPkgIds = null;
	h.bar.availablePkgIds = null;
	usePinsStore.setState({ pins: [], sections: [], hydrated: true, hydrate: async () => {} });
	useRailAutoStore.setState({ hidden: new Set(), hydrated: false });
	usePaneStore.setState({ navigateFocused } as never);
	navigateFocused.mockClear();
});
afterEach(() => {
	cleanup();
	vi.restoreAllMocks();
});

describe('Outbox approvals rail icon (R-1)', () => {
	it('is hidden when no approvals backend is active', async () => {
		mount();
		await act(async () => {});
		expect(items()).not.toContain('outbox-approvals');
	});

	it('shows right after Ngwa, above the pins, when the outbound pkg is enabled', async () => {
		h.installed = [{ id: 'com.ikenga.outbound', enabled: true }];
		usePinsStore.setState({ pins: [pin('wiki', '/pkg/wiki')] });
		mount();
		await screen.findByLabelText('Outbox approvals');
		expect(items()).toEqual(['project', 'chi', 'ngwa', 'outbox-approvals', 'pin:wiki', 'settings']);
	});

	it('shows when drafts exist even without the pkg', async () => {
		h.drafts = [{ id: 1 }];
		mount();
		await screen.findByLabelText('Outbox approvals');
	});

	it('is hidden when the outbound pkg is installed but disabled', async () => {
		h.installed = [{ id: 'com.ikenga.outbound', enabled: false }];
		mount();
		await act(async () => {});
		await new Promise((r) => setTimeout(r, 20));
		expect(items()).not.toContain('outbox-approvals');
	});

	it('inside a share: an operator sees it, a reviewer does not', async () => {
		h.installed = [{ id: 'com.ikenga.outbound', enabled: true }];
		h.share = { role: 'operator' };
		const first = mount();
		await screen.findByLabelText('Outbox approvals');
		first.unmount();
		h.share = { role: 'reviewer' };
		mount();
		await act(async () => {});
		await new Promise((r) => setTimeout(r, 20));
		expect(items()).not.toContain('outbox-approvals');
	});

	it('shows the pending count as a badge only above zero', async () => {
		h.installed = [{ id: 'com.ikenga.outbound', enabled: true }];
		const first = mount();
		await screen.findByLabelText('Outbox approvals');
		expect(screen.queryByTestId('outbox-pending-badge')).toBeNull();
		first.unmount();
		h.pending = 4;
		mount();
		const btn = await screen.findByLabelText('Outbox approvals (4 pending)');
		expect(screen.getByTestId('outbox-pending-badge').textContent).toBe('4');
		expect(btn.getAttribute('data-rail-item')).toBe('outbox-approvals');
	});

	it('click navigates the focused pane to /outbox/approvals', async () => {
		h.installed = [{ id: 'com.ikenga.outbound', enabled: true }];
		mount();
		fireEvent.click(await screen.findByLabelText('Outbox approvals'));
		expect(navigateFocused).toHaveBeenCalledWith('/outbox/approvals');
	});

	it('is the current page while the focused pane is under /outbox/', async () => {
		h.installed = [{ id: 'com.ikenga.outbound', enabled: true }];
		const btn0 = mount();
		const btn = await screen.findByLabelText('Outbox approvals');
		expect(btn.getAttribute('aria-current')).toBeNull();
		btn0.unmount();
		const { root } = usePaneStore.getState();
		usePaneStore.setState({
			root: {
				...(root as object),
				tabs: [{ kind: 'route', path: '/outbox/approvals' }],
				activeTabIdx: 0,
			},
		} as never);
		mount();
		const again = await screen.findByLabelText('Outbox approvals');
		expect(again.getAttribute('aria-current')).toBe('page');
	});
});

describe('automatic app pkg icons (R-3)', () => {
	it('one icon per installed + enabled pkg with a view', async () => {
		setPkgs([{ id: 'alpha' }, { id: 'beta' }]);
		mount();
		await screen.findByLabelText('ALPHA');
		expect(items()).toEqual(['project', 'chi', 'ngwa', 'app:alpha', 'app:beta', 'settings']);
	});

	it('a disabled pkg gets no icon', async () => {
		setPkgs([{ id: 'alpha' }, { id: 'beta', enabled: false }]);
		mount();
		await screen.findByLabelText('ALPHA');
		expect(items()).not.toContain('app:beta');
	});

	it('an uninstalled pkg loses its icon', async () => {
		setPkgs([{ id: 'alpha' }]);
		const view = mount();
		await screen.findByLabelText('ALPHA');
		view.unmount();
		setPkgs([]);
		mount();
		await act(async () => {});
		expect(items().some((i) => i.startsWith('app:'))).toBe(false);
	});

	it('no icon until the snapshot says which pkgs are enabled', async () => {
		h.bar.entries = [entry('alpha')];
		h.bar.enabledPkgIds = null;
		mount();
		await act(async () => {});
		expect(items().some((i) => i.startsWith('app:'))).toBe(false);
	});

	it('no duplicate for a pkg the user already pinned', async () => {
		setPkgs([{ id: 'alpha' }, { id: 'beta' }]);
		usePinsStore.setState({ pins: [pin('p1', '/pkg/alpha/')] });
		mount();
		await screen.findByLabelText('BETA');
		expect(items()).toEqual(['project', 'chi', 'ngwa', 'pin:p1', 'app:beta', 'settings']);
	});

	it('no duplicate for a pin_on_install pin (manifestId = pkg id)', async () => {
		setPkgs([{ id: 'com.x.alpha' }]);
		usePinsStore.setState({ pins: [pin('p1', '/somewhere', 'com.x.alpha')] });
		mount();
		await act(async () => {});
		expect(items().some((i) => i.startsWith('app:'))).toBe(false);
	});

	it('click opens the pkg view in the focused pane', async () => {
		setPkgs([{ id: 'alpha' }]);
		mount();
		fireEvent.click(await screen.findByLabelText('ALPHA'));
		expect(navigateFocused).toHaveBeenCalledWith('/pkg/alpha/');
	});

	it('Hide from rail removes the icon and persists; re-show restores it', async () => {
		setPkgs([{ id: 'alpha' }]);
		const view = mount();
		const icon = await screen.findByLabelText('ALPHA');
		fireEvent.contextMenu(icon);
		fireEvent.click(await screen.findByText('Hide from rail'));
		await waitFor(() => expect(items()).not.toContain('app:alpha'));
		expect(JSON.parse(h.kv.get(RAIL_HIDDEN_KV) ?? '[]')).toEqual(['alpha']);

		// A fresh session reads the saved list back.
		view.unmount();
		useRailAutoStore.setState({ hidden: new Set(), hydrated: false });
		mount();
		await waitFor(() => expect(useRailAutoStore.getState().hydrated).toBe(true));
		expect(items()).not.toContain('app:alpha');

		await act(async () => {
			await useRailAutoStore.getState().show('alpha');
		});
		await screen.findByLabelText('ALPHA');
		expect(JSON.parse(h.kv.get(RAIL_HIDDEN_KV) ?? 'null')).toEqual([]);
	});
});
