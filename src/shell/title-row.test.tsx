// WP-09 T1 / T6 — the title row has exactly two controls (project chip +
// branch chip), both keyboard-reachable; the branch chip reads the git pkg's
// `repo.snapshot` through `pkgSidecarCall` and hides when that fails.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render as rtlRender, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactElement } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const sidecarMock = vi.hoisted(() => vi.fn());
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	pkgSidecarCall: sidecarMock,
}));

import { usePaneStore } from '@/lib/panes/pane-store';
import { useShellStore } from '@/lib/shell/shell-store';
import type { Project } from '@/lib/tauri-cmd';
import { todayLocalDate } from '@/shell/home/daily-address';
import { GIT_BRANCHES_ROUTE, parseRepoSnapshot, TitleRow } from './title-row';

function render(ui: ReactElement) {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return rtlRender(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

const PROJECT: Project = {
	id: 'label-ops',
	display_name: 'Label Ops',
	root_path: '/home/e2e/label-ops',
	icon: null,
	color: null,
	description: null,
	position: 0,
	is_default: false,
	created_at: 1,
	archived_at: null,
} as Project;

function snapshotStdout(snapshot: Record<string, unknown>) {
	return `${JSON.stringify({ jsonrpc: '2.0', id: 1, result: { ok: true, snapshot } })}\n`;
}

/** Everything a keyboard user can land on. */
function tabbables(root: Element) {
	return root.querySelectorAll(
		'button, a[href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
	);
}

beforeEach(() => {
	sidecarMock.mockReset();
	useShellStore.setState({ projects: [PROJECT], activeProjectId: PROJECT.id });
});
afterEach(cleanup);

describe('parseRepoSnapshot', () => {
	it('reads branch + summed change counts from the last stdout line', () => {
		const out = `noise\n${snapshotStdout({
			branch: 'feat/frame-chrome',
			detached: false,
			headSha: 'abcdef1234',
			staged: 1,
			unstaged: 2,
			untracked: 3,
			conflicted: 0,
		})}`;
		expect(parseRepoSnapshot(out)).toEqual({
			branch: 'feat/frame-chrome',
			detached: false,
			modified: 6,
		});
	});

	it('falls back to the short sha when detached', () => {
		expect(
			parseRepoSnapshot(snapshotStdout({ branch: null, detached: true, headSha: 'abcdef1234' }))
		).toEqual({ branch: 'abcdef1', detached: true, modified: 0 });
	});

	it('returns null for a git error or garbage', () => {
		expect(
			parseRepoSnapshot(JSON.stringify({ result: { ok: false, kind: 'not-a-repo' } }))
		).toBeNull();
		expect(parseRepoSnapshot('not json')).toBeNull();
		expect(parseRepoSnapshot('')).toBeNull();
	});
});

describe('<TitleRow />', () => {
	it('T1: renders exactly two controls — project chip and branch chip', async () => {
		sidecarMock.mockResolvedValue({
			ok: true,
			stdout: snapshotStdout({ branch: 'main', detached: false, staged: 0, unstaged: 3 }),
		});
		render(<TitleRow />);
		await screen.findByTestId('title-branch-chip');
		const row = screen.getByTestId('title-row');
		expect(tabbables(row)).toHaveLength(2);
		expect(screen.getByTestId('title-project-chip').textContent).toContain('Label Ops');
		expect(screen.getByTestId('title-branch-chip').textContent).toContain('main');
		// The sidecar was asked for `repo.snapshot` on the active project's root.
		const stdin = JSON.parse(sidecarMock.mock.calls[0]![3].stdin);
		expect(stdin.method).toBe('repo.snapshot');
		expect(stdin.params).toEqual({ repo: PROJECT.root_path });
	});

	it('hides the branch chip entirely when the git pkg is absent', async () => {
		sidecarMock.mockResolvedValue({ ok: false, stdout: '' });
		render(<TitleRow />);
		await waitFor(() => expect(sidecarMock).toHaveBeenCalledTimes(2));
		expect(screen.queryByTestId('title-branch-chip')).toBeNull();
		expect(tabbables(screen.getByTestId('title-row'))).toHaveLength(1);
	});

	it('T6: both chips are reachable by Tab and operable by keyboard', async () => {
		const navigateFocused = vi.fn();
		usePaneStore.setState({ navigateFocused });
		sidecarMock.mockResolvedValue({
			ok: true,
			stdout: snapshotStdout({ branch: 'main', detached: false }),
		});
		const user = userEvent.setup();
		render(<TitleRow />);
		await screen.findByTestId('title-branch-chip');

		await user.tab();
		expect(document.activeElement).toBe(screen.getByTestId('title-project-chip'));
		await user.tab();
		expect(document.activeElement).toBe(screen.getByTestId('title-branch-chip'));
		await user.keyboard('{Enter}');
		expect(navigateFocused).toHaveBeenCalledWith(GIT_BRANCHES_ROUTE);

		// Project chip opens the switcher popover from the keyboard.
		screen.getByTestId('title-project-chip').focus();
		await user.keyboard('{Enter}');
		expect(await screen.findByText('Switch project')).toBeTruthy();
	});
});

// WP-39 — the one control the title row gains beyond T1's two: hidden by
// default (T1 above still sees exactly two), shown only while the daily
// address is dismissed for today.
describe('daily address reopen (WP-39)', () => {
	beforeEach(() => {
		useShellStore.setState({ dailyAddressDismissedOn: null });
	});

	it('is absent when not dismissed today — T1 still holds', async () => {
		sidecarMock.mockResolvedValue({ ok: false, stdout: '' });
		render(<TitleRow />);
		await waitFor(() => expect(sidecarMock).toHaveBeenCalled());
		expect(screen.queryByTestId('title-daily-address-reopen')).toBeNull();
	});

	it('appears once dismissed today, and reopening clears the dismissal', async () => {
		useShellStore.setState({ dailyAddressDismissedOn: todayLocalDate() });
		sidecarMock.mockResolvedValue({ ok: false, stdout: '' });
		const user = userEvent.setup();
		render(<TitleRow />);
		await waitFor(() => expect(sidecarMock).toHaveBeenCalled());

		const reopen = screen.getByTestId('title-daily-address-reopen');
		expect(reopen).toBeInTheDocument();
		await user.click(reopen);
		expect(useShellStore.getState().dailyAddressDismissedOn).toBeNull();
	});

	it('stays absent for a past dismissal date (only today suppresses it)', async () => {
		useShellStore.setState({ dailyAddressDismissedOn: '2000-01-01' });
		sidecarMock.mockResolvedValue({ ok: false, stdout: '' });
		render(<TitleRow />);
		await waitFor(() => expect(sidecarMock).toHaveBeenCalled());
		expect(screen.queryByTestId('title-daily-address-reopen')).toBeNull();
	});
});
