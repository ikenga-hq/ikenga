// WP-39 — daily address: the once-per-day dismiss/reopen contract, plus the
// pure `todayLocalDate()` helper. Every tile's data source is mocked at the
// module boundary `daily-address.tsx` itself imports (chiList, the
// notifications query surface, the updater/pkg-update hooks, listTodos) so
// this stays a unit test of the dismiss/reopen + empty-state wiring, not an
// integration test of those other WPs' own modules.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactElement } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@/lib/panes/pane-store', () => {
	const state = { navigateFocused: vi.fn() };
	const usePaneStore = (selector: (s: typeof state) => unknown) => selector(state);
	(usePaneStore as unknown as { getState: () => typeof state }).getState = () => state;
	return { usePaneStore };
});

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	chiList: vi.fn().mockResolvedValue([]),
}));

vi.mock('@/lib/iyke/memory', () => ({
	listTodos: vi.fn().mockResolvedValue({ scope: 'project:p1', todos: [] }),
	completeTodo: vi.fn().mockResolvedValue({ id: 't1', completed_at: Date.now() }),
}));

vi.mock('@/lib/queries/notifications', () => ({
	notificationsListQueryOptions: () => ({
		queryKey: ['mock', 'notifications'],
		queryFn: () => Promise.resolve([]),
	}),
	useMarkNotificationsRead: () => ({ mutate: vi.fn() }),
	useMarkAllNotificationsRead: () => ({ mutate: vi.fn() }),
}));

vi.mock('@/lib/pkgs/use-derived', () => ({
	usePkgsDerived: () => ({ updates: [], isLoading: false }),
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
		check: vi.fn().mockResolvedValue(undefined),
		install: vi.fn().mockResolvedValue(undefined),
		restart: vi.fn().mockResolvedValue(undefined),
	}),
}));

import { useShellStore } from '@/lib/shell/shell-store';
import { DailyAddress, todayLocalDate } from './daily-address';

function renderAddress(ui: ReactElement = <DailyAddress />) {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

beforeEach(() => {
	useShellStore.setState({ activeProjectId: 'p1', dailyAddressDismissedOn: null });
});
afterEach(cleanup);

describe('todayLocalDate', () => {
	it('formats a local date as YYYY-MM-DD, zero-padded', () => {
		expect(todayLocalDate(new Date(2026, 0, 5))).toBe('2026-01-05');
		expect(todayLocalDate(new Date(2026, 8, 25))).toBe('2026-09-25');
	});

	it('does not use UTC (would drift near midnight in non-UTC zones)', () => {
		// A date constructed from local components must round-trip through the
		// same local components, not through `toISOString()`'s UTC day.
		const d = new Date(2026, 5, 1, 0, 30);
		expect(todayLocalDate(d)).toBe('2026-06-01');
	});
});

describe('<DailyAddress/>', () => {
	it('renders data-state="daily-address" by default (not dismissed today)', async () => {
		renderAddress();
		await waitFor(() => expect(screen.getByLabelText('Daily address')).toBeInTheDocument());
		expect(document.querySelector('[data-state="daily-address"]')).not.toBeNull();
	});

	it('dismissing hides it and persists today\'s date on the shell store', async () => {
		const user = userEvent.setup();
		renderAddress();
		await waitFor(() => expect(screen.getByLabelText('Daily address')).toBeInTheDocument());

		await user.click(screen.getByLabelText('Dismiss daily address'));

		expect(screen.queryByLabelText('Daily address')).not.toBeInTheDocument();
		expect(useShellStore.getState().dailyAddressDismissedOn).toBe(todayLocalDate());
	});

	it('a past dismissal date still shows it (only today\'s date suppresses it)', async () => {
		useShellStore.setState({ dailyAddressDismissedOn: '2000-01-01' });
		renderAddress();
		await waitFor(() => expect(screen.getByLabelText('Daily address')).toBeInTheDocument());
	});

	it('reopening (clearing the dismissal) shows it again', () => {
		useShellStore.setState({ dailyAddressDismissedOn: todayLocalDate() });
		renderAddress();
		expect(screen.queryByLabelText('Daily address')).not.toBeInTheDocument();

		useShellStore.getState().setDailyAddressDismissed(null);
		renderAddress();
		expect(screen.getByLabelText('Daily address')).toBeInTheDocument();
	});
});
