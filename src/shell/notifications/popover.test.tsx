// WP-40b — `NotificationsPopoverContent`: Today/Earlier grouping, the empty
// state (WP-43's `EmptyState`), mark-all-read, and the per-kind mute menu
// (permission/violation excluded per D-07). Renders the popover body
// directly rather than driving it through the `Popover` trigger — the
// trigger/portal mechanics are Radix's, already exercised elsewhere in this
// tree; this test is about what WP-40b built on top of it.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render as rtlRender, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactElement } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { NotificationRow, NotificationsMuteState } from '@/lib/tauri-cmd';

const mocks = vi.hoisted(() => ({
	notificationsList: vi.fn(),
	notificationsUnreadCount: vi.fn(),
	notificationsMuteState: vi.fn(),
	notificationsMarkRead: vi.fn(),
	notificationsMarkAllRead: vi.fn(),
	notificationsMuteKind: vi.fn(),
	notificationsUnmuteKind: vi.fn(),
	navigateFocused: vi.fn(),
	addTab: vi.fn(),
	accessStatus: vi.fn(),
	accessRoutingGet: vi.fn(),
}));

vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	accessStatus: mocks.accessStatus,
	accessRoutingGet: mocks.accessRoutingGet,
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	notificationsList: mocks.notificationsList,
	notificationsUnreadCount: mocks.notificationsUnreadCount,
	notificationsMuteState: mocks.notificationsMuteState,
	notificationsMarkRead: mocks.notificationsMarkRead,
	notificationsMarkAllRead: mocks.notificationsMarkAllRead,
	notificationsMuteKind: mocks.notificationsMuteKind,
	notificationsUnmuteKind: mocks.notificationsUnmuteKind,
}));

vi.mock('@/lib/panes/pane-store', () => ({
	usePaneStore: {
		getState: () => ({
			focusedId: 'pane-1',
			navigateFocused: mocks.navigateFocused,
			addTab: mocks.addTab,
		}),
	},
}));

vi.mock('@/lib/iyke/client', () => ({
	iykeFetch: vi.fn(() => Promise.resolve(new Response(null, { status: 204 }))),
}));

import { setHostDecideBlock } from './actions';
import { NotificationsPopoverContent } from './popover';

function render(ui: ReactElement) {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return rtlRender(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

const NOW = Date.parse('2026-09-25T12:00:00Z');

function row(overrides: Partial<NotificationRow>): NotificationRow {
	return {
		id: 1,
		kind: 'permission',
		title: 'claude wants to read .env',
		body: 'session 3 · royalti-co',
		action: null,
		source: 'iyke.hooks',
		dedupeKey: null,
		count: 1,
		createdAt: NOW,
		updatedAt: NOW,
		readAt: null,
		...overrides,
	};
}

const MUTE_STATE: NotificationsMuteState = {
	muted: ['update'],
	mutable: ['run_finished', 'run_failed', 'update', 'invite', 'system'],
};

beforeEach(() => {
	vi.setSystemTime(NOW);
	for (const fn of Object.values(mocks)) fn.mockReset();
	mocks.notificationsMuteState.mockResolvedValue(MUTE_STATE);
	mocks.notificationsMarkAllRead.mockResolvedValue(0);
	mocks.notificationsMarkRead.mockResolvedValue(0);
	mocks.accessStatus.mockResolvedValue(null);
	setHostDecideBlock(null);
});

afterEach(() => {
	cleanup();
	vi.useRealTimers();
});

describe('NotificationsPopoverContent', () => {
	it('renders the empty state and its one action opens Notification settings + closes', async () => {
		mocks.notificationsList.mockResolvedValue([]);
		const onClose = vi.fn();
		render(<NotificationsPopoverContent onClose={onClose} />);

		const empty = await screen.findByText('Nothing yet');
		expect(empty.closest('[data-state="notifications-empty"]')).toBeTruthy();

		await userEvent.click(screen.getByRole('button', { name: 'Notification settings' }));
		expect(onClose).toHaveBeenCalledOnce();
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/settings/workspace');
	});

	it('groups rows into Today / Earlier by updatedAt', async () => {
		const dayMs = 24 * 60 * 60 * 1000;
		mocks.notificationsList.mockResolvedValue([
			row({ id: 1, title: 'today row', updatedAt: NOW }),
			row({ id: 2, kind: 'update', title: 'earlier row', updatedAt: NOW - 2 * dayMs }),
		]);
		render(<NotificationsPopoverContent onClose={vi.fn()} />);

		expect(await screen.findByText('today row')).toBeTruthy();
		expect(screen.getByText('Today')).toBeTruthy();
		expect(screen.getByText('earlier row')).toBeTruthy();
		expect(screen.getByText('Earlier')).toBeTruthy();
	});

	it('Mark all read calls the mutation with no kind filter', async () => {
		mocks.notificationsList.mockResolvedValue([row({})]);
		render(<NotificationsPopoverContent onClose={vi.fn()} />);

		await screen.findByText('claude wants to read .env');
		await userEvent.click(screen.getByRole('button', { name: 'Mark all read' }));
		await waitFor(() => expect(mocks.notificationsMarkAllRead).toHaveBeenCalledWith(null));
	});

	it('clicking a row action marks that row read', async () => {
		mocks.notificationsList.mockResolvedValue([
			row({
				id: 7,
				kind: 'violation',
				title: 'blocked ffmpeg',
				action: { kind: 'open.violations', pkgId: 'com.ikenga.pkg-browser' },
			}),
		]);
		render(<NotificationsPopoverContent onClose={vi.fn()} />);

		await userEvent.click(await screen.findByRole('button', { name: 'Review' }));
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/packages?filter=review');
		await waitFor(() => expect(mocks.notificationsMarkRead).toHaveBeenCalledWith([7]));
	});

	it('a WSL network row is tagged with its own "system" kind (D-19)', async () => {
		mocks.notificationsList.mockResolvedValue([
			row({
				id: 9,
				kind: 'system',
				title: 'WSL has no network · Ubuntu',
				action: { kind: 'fix.wsl_network', distro: 'Ubuntu', state: 'no_route' },
				dedupeKey: 'wsl:network:ubuntu',
			}),
		]);
		render(<NotificationsPopoverContent onClose={vi.fn()} />);

		await screen.findByText('WSL has no network · Ubuntu');
		expect(screen.getByText('system')).toBeTruthy();
		expect(screen.queryByText('violation')).toBeNull();
		expect(screen.queryByText('wsl network')).toBeNull();
	});

	it('the mute menu offers only mutable kinds and reflects current mute state', async () => {
		mocks.notificationsList.mockResolvedValue([row({})]);
		render(<NotificationsPopoverContent onClose={vi.fn()} />);

		await screen.findByText('claude wants to read .env');
		await userEvent.click(screen.getByRole('button', { name: 'Mute a kind' }));

		const menu = await screen.findByRole('menu');
		for (const label of ['run finished', 'run failed', 'update', 'invite', 'system']) {
			expect(within(menu).getByText(label)).toBeTruthy();
		}
		expect(within(menu).queryByText('permission')).toBeNull();
		expect(within(menu).queryByText('violation')).toBeNull();
		expect(within(menu).getByText(/cannot be muted/i)).toBeTruthy();

		const updateItem = within(menu).getByText('update').closest('[role="menuitemcheckbox"]');
		expect(updateItem?.getAttribute('aria-checked')).toBe('true');

		await userEvent.click(within(menu).getByText('run finished'));
		expect(mocks.notificationsMuteKind).toHaveBeenCalledWith('run_finished');
	});

	// G-ACCESS §5.7 (WP-78a): a routed-away live ask shows where it is
	// answered — and no dead Allow / Deny.
	it('a routed-away ask shows its reason instead of Allow / Deny', async () => {
		mocks.accessStatus.mockResolvedValue({ store: 'ok', caps: ['files', 'sessions'] });
		mocks.accessRoutingGet.mockResolvedValue({
			mode: 'this_device',
			deviceId: 'd',
			deviceName: 'ned-desktop',
		});
		const decide = {
			kind: 'permission.decide',
			via: 'hooks',
			requestId: 'r',
			terminalId: 't-1',
		} as const;
		mocks.notificationsList.mockResolvedValue([
			row({ id: 3, title: 'live ask', action: decide }),
			row({ id: 4, title: 'over ask', action: decide, resolvedAt: NOW, readAt: NOW }),
		]);
		render(<NotificationsPopoverContent onClose={vi.fn()} />);
		const reason = await screen.findByText('Answer on ned-desktop (this device only)');
		expect(reason.closest('[data-notification-row="3"]')).toBeTruthy();
		expect(document.querySelectorAll('[data-state="notification-routed-away"]')).toHaveLength(1);
		expect(screen.queryByRole('button', { name: 'Allow once' })).toBeNull();
		expect(screen.getAllByRole('button', { name: 'Open terminal' })).toHaveLength(2);
	});

	it('an ask this device may answer shows Allow / Deny and no reason', async () => {
		mocks.accessStatus.mockResolvedValue({ store: 'ok', caps: ['files', 'approve'] });
		mocks.notificationsList.mockResolvedValue([
			row({
				id: 5,
				title: 'answerable',
				action: { kind: 'permission.decide', via: 'hooks', requestId: 'r', terminalId: 't' },
			}),
		]);
		render(<NotificationsPopoverContent onClose={vi.fn()} />);
		expect(await screen.findByRole('button', { name: 'Allow once' })).toBeTruthy();
		expect(document.querySelector('[data-state="notification-routed-away"]')).toBeNull();
	});
});
