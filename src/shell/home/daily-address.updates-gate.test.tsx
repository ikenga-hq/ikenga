// Gap audit rank 3 — the daily address's Updates tile "Update all" must not
// offer an install the daemon cannot run: in a browser session it is disabled
// and reads "Not available on this server yet". The desktop keeps it working.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({ remote: false }));

vi.mock('@tanstack/react-router', () => ({ useNavigate: () => vi.fn() }));
vi.mock('@/lib/settings/client', () => ({
	readSettingsFile: vi.fn(() => Promise.resolve({ effective: { workspace: {} } })),
}));
vi.mock('@/lib/iyke/client', () => ({ iykeFetch: vi.fn() }));
vi.mock('@/lib/transport/dialog-shim', () => ({ confirm: vi.fn(() => Promise.resolve(true)) }));
vi.mock('@/lib/panes/pane-store', () => {
	const state = { navigateFocused: vi.fn(), addTab: vi.fn(), focusedId: 'pane-1' };
	const usePaneStore = (selector: (s: typeof state) => unknown) => selector(state);
	(usePaneStore as unknown as { getState: () => typeof state }).getState = () => state;
	return { usePaneStore };
});
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => h.remote,
	chiList: vi.fn(() => Promise.resolve([])),
	notificationsList: vi.fn(() => Promise.resolve([])),
}));
vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	accessStatus: vi.fn(() => Promise.resolve(null)),
}));
vi.mock('@/lib/iyke/memory', () => ({
	listTodos: vi.fn().mockResolvedValue({ scope: 'project:p1', todos: [] }),
	completeTodo: vi.fn(),
}));
vi.mock('@/lib/queries/notifications', async (orig) => ({
	...(await orig<typeof import('@/lib/queries/notifications')>()),
	notificationsListQueryOptions: () => ({
		queryKey: ['mock', 'notifications'],
		queryFn: () => Promise.resolve([]),
	}),
	useMarkNotificationsRead: () => ({ mutate: vi.fn() }),
	useMarkAllNotificationsRead: () => ({ mutate: vi.fn() }),
	invalidateNotifications: vi.fn(() => Promise.resolve()),
}));
vi.mock('@/lib/pkgs/use-derived', () => ({
	usePkgsDerived: () => ({
		updates: [{ id: 'com.example.a', name: 'A', version: '1.0.0', latest: '2.0.0' }],
		isLoading: false,
	}),
}));
vi.mock('@/lib/pkgs/use-update-pkgs', () => ({
	useUpdatePkgs: () => ({ mutate: vi.fn(), isPending: false }),
}));
vi.mock('@/lib/updater/use-updater', () => ({
	useUpdater: () => ({
		available: null,
		installing: false,
		installed: false,
		checking: false,
		check: vi.fn(),
		install: vi.fn(),
		restart: vi.fn(),
	}),
}));

import { DailyAddress } from './daily-address';

function renderAddress() {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={client}>
			<DailyAddress />
		</QueryClientProvider>
	);
}

afterEach(() => {
	cleanup();
	h.remote = false;
});

describe('daily address Updates tile — install gate (gap rank 3)', () => {
	it('disables "Update all" with the honest reason in a remote session', async () => {
		h.remote = true;
		renderAddress();
		const btn = await screen.findByRole('button', { name: 'Not available on this server yet' });
		expect((btn as HTMLButtonElement).disabled).toBe(true);
		expect(screen.queryByRole('button', { name: 'Update all' })).toBeNull();
	});

	it('keeps "Update all" working on the desktop', async () => {
		renderAddress();
		const btn = await screen.findByRole('button', { name: 'Update all' });
		expect((btn as HTMLButtonElement).disabled).toBe(false);
	});
});
