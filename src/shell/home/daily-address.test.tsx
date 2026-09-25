// WP-39 — daily address: the once-per-day dismiss/reopen contract, plus the
// pure `todayLocalDate()` helper. Every tile's data source is mocked at the
// module boundary `daily-address.tsx` itself imports (chiList, the
// notifications query surface, the updater/pkg-update hooks, listTodos) so
// this stays a unit test of the dismiss/reopen + empty-state wiring, not an
// integration test of those other WPs' own modules. Review fixes covered
// here: "Waiting on you" = unresolved permission rows (Open never marks
// read), runs scoped to the active project root, the `workspace.dailyAddress`
// setting, the greeting and the two footer actions.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactElement } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ChiCacheRow, NotificationRow } from '@/lib/tauri-cmd';

const mocks = vi.hoisted(() => ({
	permissionRows: [] as unknown[],
	markRead: vi.fn(),
	addTab: vi.fn(),
	navigateFocused: vi.fn(),
	navigate: vi.fn(),
	iykeFetch: vi.fn(() => Promise.resolve(new Response(null, { status: 204 }))),
	settings: { effective: { workspace: {} } } as unknown,
	chiList: vi.fn(),
}));

vi.mock('@tanstack/react-router', () => ({
	useNavigate: () => mocks.navigate,
}));

vi.mock('@/lib/settings/client', () => ({
	readSettingsFile: vi.fn(() => Promise.resolve(mocks.settings)),
}));

vi.mock('@/lib/iyke/client', () => ({
	iykeFetch: mocks.iykeFetch,
}));

vi.mock('@/lib/transport/dialog-shim', () => ({
	confirm: vi.fn(() => Promise.resolve(true)),
}));

vi.mock('@/lib/panes/pane-store', () => {
	const state = { navigateFocused: mocks.navigateFocused, addTab: mocks.addTab, focusedId: 'pane-1' };
	const usePaneStore = (selector: (s: typeof state) => unknown) => selector(state);
	(usePaneStore as unknown as { getState: () => typeof state }).getState = () => state;
	return { usePaneStore };
});

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	chiList: mocks.chiList,
}));

vi.mock('@/lib/iyke/memory', () => ({
	listTodos: vi.fn().mockResolvedValue({ scope: 'project:p1', todos: [] }),
	completeTodo: vi.fn().mockResolvedValue({ id: 't1', completed_at: Date.now() }),
}));

vi.mock('@/lib/queries/notifications', async (orig) => ({
	...(await orig<typeof import('@/lib/queries/notifications')>()),
	notificationsListQueryOptions: (filter: { kinds?: string[] }) => ({
		queryKey: ['mock', 'notifications', filter.kinds?.join(',') ?? ''],
		queryFn: () =>
			Promise.resolve(filter.kinds?.includes('permission') ? mocks.permissionRows : []),
	}),
	useMarkNotificationsRead: () => ({ mutate: mocks.markRead }),
	useMarkAllNotificationsRead: () => ({ mutate: vi.fn() }),
	invalidateNotifications: vi.fn(() => Promise.resolve()),
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
import {
	DailyAddress,
	greetingFor,
	isUnderProjectRoot,
	pendingPermissions,
	projectRecentRuns,
	summarizeDay,
	todayLocalDate,
} from './daily-address';

function permissionRow(overrides: Partial<NotificationRow>): NotificationRow {
	return {
		id: 1,
		kind: 'permission',
		title: 'Claude wants to use Bash',
		body: null,
		action: null,
		source: 'iyke.hooks',
		dedupeKey: null,
		count: 1,
		createdAt: Date.now(),
		updatedAt: Date.now(),
		readAt: null,
		resolvedAt: null,
		...overrides,
	};
}

function run(overrides: Partial<ChiCacheRow>): ChiCacheRow {
	return {
		run_id: 'r1',
		engine_id: 'claude-code',
		owner: 'chi',
		status: 'done',
		ended_at: new Date().toISOString(),
		...overrides,
	};
}

function renderAddress(ui: ReactElement = <DailyAddress />) {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

beforeEach(() => {
	useShellStore.setState({ activeProjectId: 'p1', projects: [], dailyAddressDismissedOn: null });
	mocks.permissionRows = [];
	mocks.settings = { effective: { workspace: {} } };
	mocks.chiList.mockReset().mockResolvedValue([]);
	mocks.markRead.mockClear();
	mocks.addTab.mockClear();
	mocks.navigateFocused.mockClear();
	mocks.navigate.mockClear();
	mocks.iykeFetch.mockClear();
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

	it('reopening (clearing the dismissal) shows it again', async () => {
		useShellStore.setState({ dailyAddressDismissedOn: todayLocalDate() });
		renderAddress();
		expect(screen.queryByLabelText('Daily address')).not.toBeInTheDocument();

		useShellStore.getState().setDailyAddressDismissed(null);
		renderAddress();
		expect(await screen.findByLabelText('Daily address')).toBeInTheDocument();
	});

	it('is hidden when workspace.dailyAddress is off in settings.json', async () => {
		mocks.settings = { effective: { workspace: { dailyAddress: false } } };
		renderAddress();
		// Let the settings read settle, then the address must still be absent.
		await new Promise((r) => setTimeout(r, 0));
		await new Promise((r) => setTimeout(r, 0));
		expect(screen.queryByLabelText('Daily address')).not.toBeInTheDocument();
		expect(mocks.chiList).not.toHaveBeenCalled();
	});

	it('footer: Settings › Workspace and Run consecration again are real actions', async () => {
		const user = userEvent.setup();
		const resetOnboarding = vi.fn();
		useShellStore.setState({ resetOnboarding });
		renderAddress();
		await screen.findByLabelText('Daily address');

		await user.click(screen.getByRole('button', { name: /Settings › Workspace/ }));
		expect(mocks.navigateFocused).toHaveBeenCalledWith('/settings/workspace');

		await user.click(screen.getByRole('button', { name: /Run consecration again/ }));
		await waitFor(() => expect(mocks.navigate).toHaveBeenCalledWith({ to: '/onboarding' }));
		expect(resetOnboarding).toHaveBeenCalled();
	});

	it('greets the user by name', async () => {
		useShellStore.setState({ userName: 'nedjamez' });
		renderAddress();
		expect(await screen.findByText(/, nedjamez\./)).toBeInTheDocument();
	});
});

describe('waiting on you (pending permissions)', () => {
	it('pendingPermissions keeps unresolved rows whether or not they were read', () => {
		const rows = [
			permissionRow({ id: 1 }),
			permissionRow({ id: 2, readAt: 5 }),
			permissionRow({ id: 3, resolvedAt: 9, readAt: 9 }),
		];
		expect(pendingPermissions(rows).map((r) => r.id)).toEqual([1, 2]);
	});

	it('a hooks-gate ask offers Allow once / Deny inline and posts the decision', async () => {
		mocks.permissionRows = [
			permissionRow({
				action: { kind: 'permission.decide', via: 'hooks', requestId: 'req-1', terminalId: 't-1' },
			}),
		];
		const user = userEvent.setup();
		renderAddress();
		await user.click(await screen.findByRole('button', { name: 'Allow once' }));
		expect(mocks.iykeFetch).toHaveBeenCalledWith(
			'/iyke/hooks/decision',
			expect.objectContaining({ body: JSON.stringify({ requestId: 'req-1', decision: 'approved' }) })
		);
		expect(mocks.markRead).not.toHaveBeenCalled();
	});

	it('Open on a pending terminal prompt goes to the terminal and does NOT mark it read', async () => {
		mocks.permissionRows = [
			permissionRow({ action: { kind: 'open.terminal', terminalId: 'term-9', sessionId: null } }),
		];
		const user = userEvent.setup();
		renderAddress();
		await user.click(await screen.findByRole('button', { name: 'Open' }));
		expect(mocks.addTab).toHaveBeenCalledWith('pane-1', { kind: 'terminal', sessionId: 'term-9' });
		expect(mocks.markRead).not.toHaveBeenCalled();
	});

	it('a resolved ask is not shown as waiting', async () => {
		mocks.permissionRows = [permissionRow({ resolvedAt: Date.now(), readAt: Date.now() })];
		renderAddress();
		await waitFor(() =>
			expect(document.querySelector('[data-state="daily-address-permissions-empty"]')).not.toBeNull()
		);
	});
});

describe('since you were last here (project-scoped runs)', () => {
	it('isUnderProjectRoot matches the root and descendants, not prefix siblings', () => {
		expect(isUnderProjectRoot('/code/app', '/code/app')).toBe(true);
		expect(isUnderProjectRoot('/code/app/sub', '/code/app/')).toBe(true);
		expect(isUnderProjectRoot('/code/app2', '/code/app')).toBe(false);
		expect(isUnderProjectRoot('C:\\code\\app\\x', 'C:/code/app')).toBe(true);
		expect(isUnderProjectRoot(undefined, '/code/app')).toBe(false);
	});

	it('projectRecentRuns keeps only runs under the active project root', () => {
		const rows = [
			run({ run_id: 'in', cwd: '/code/app/pkg' }),
			run({ run_id: 'out', cwd: '/code/other' }),
			run({ run_id: 'nocwd' }),
		];
		expect(projectRecentRuns(rows, '/code/app').map((r) => r.run_id)).toEqual(['in']);
		expect(projectRecentRuns(rows, null).map((r) => r.run_id)).toEqual(['in', 'out', 'nocwd']);
	});

	it('the runs tile shows only the active project\'s runs', async () => {
		useShellStore.setState({
			activeProjectId: 'p1',
			projects: [
				{ id: 'p1', display_name: 'App', root_path: '/code/app', icon: null } as never,
			],
		});
		mocks.chiList.mockResolvedValue([
			run({ run_id: 'in', brief: 'in-project run', cwd: '/code/app' }),
			run({ run_id: 'out', brief: 'other project run', cwd: '/code/other' }),
		]);
		renderAddress();
		expect(await screen.findByText('in-project run')).toBeInTheDocument();
		expect(screen.queryByText('other project run')).not.toBeInTheDocument();
	});
});

describe('greeting', () => {
	it('greetingFor picks the time of day', () => {
		expect(greetingFor(8)).toBe('Good morning');
		expect(greetingFor(14)).toBe('Good afternoon');
		expect(greetingFor(21)).toBe('Good evening');
	});

	it('summarizeDay leaves out what has not loaded', () => {
		expect(summarizeDay({})).toBeNull();
		expect(
			summarizeDay({
				runs: [run({}), run({ status: 'running' })],
				pending: 1,
				openTodos: 4,
			})
		).toBe(
			'Your Chi ran 1 thing since yesterday, 1 is still going, 1 permission is waiting on you, and 4 todos are open.'
		);
		expect(summarizeDay({ openTodos: 0 })).toBe('0 todos are open.');
	});
});
