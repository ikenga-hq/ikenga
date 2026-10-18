// The project chip also lists projects other people shared with you (T1
// remote sessions) and, inside a share, shows the shared project with a
// "shared" marker plus a "Leave shared project" item.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render as rtlRender, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ReactElement } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const accessStatusMock = vi.hoisted(() => vi.fn());
const accessSharesListMock = vi.hoisted(() => vi.fn());
vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	accessStatus: accessStatusMock,
	accessSharesList: accessSharesListMock,
}));

const outsideShareMock = vi.hoisted(() => vi.fn());
const remoteMock = vi.hoisted(() => vi.fn());
vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	invokeOutsideShare: outsideShareMock,
	isRemoteWebSession: remoteMock,
}));

const openShareMock = vi.hoisted(() => vi.fn());
const leaveShareMock = vi.hoisted(() => vi.fn());
vi.mock('./people/shared-with-you', async (orig) => ({
	...(await orig<typeof import('./people/shared-with-you')>()),
	openShare: openShareMock,
	leaveShare: leaveShareMock,
}));

const projectSetActiveMock = vi.hoisted(() => vi.fn());
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	projectSetActive: projectSetActiveMock,
	pkgSidecarCall: vi.fn().mockResolvedValue({ ok: false, stdout: '' }),
	isRemoteWebSession: vi.fn(() => false),
	gitStatus: vi.fn(),
}));

import { useShellStore } from '@/lib/shell/shell-store';
import type { Project } from '@/lib/tauri-cmd';
import { __resetShareModeForTests, currentShare, setShareMode } from '@/lib/transport';
import { __resetSharedProjectsForTests } from './people/use-shared-projects';
import { TitleRow } from './title-row';

function render(ui: ReactElement) {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return rtlRender(<QueryClientProvider client={client}>{ui}</QueryClientProvider>);
}

const mk = (id: string, name: string, position: number): Project =>
	({
		id,
		display_name: name,
		root_path: `/home/e2e/${id}`,
		icon: null,
		color: null,
		description: null,
		position,
		is_default: false,
		created_at: position,
		archived_at: null,
	}) as Project;

const MINE = mk('label-ops', 'Label Ops', 0);
const OTHER = mk('catalog', 'Catalog', 1);

const OWNER = '11111111-1111-1111-1111-111111111111';
const SHARE_VIEW = {
	projectKey: `${OWNER}/royalti-co`,
	ownerPrincipalId: OWNER,
	ownerUsername: 'ada',
	projectId: 'royalti-co',
	projectName: 'royalti-co',
	role: 'reviewer' as const,
	scope: 'project' as const,
	artifactPath: null,
	expiresAt: null,
	addedAt: 1,
};

const assignMock = vi.fn();
const realLocation = window.location;
const realSetActive = useShellStore.getState().setActiveProject;

beforeEach(() => {
	accessStatusMock.mockReset();
	accessSharesListMock.mockReset();
	openShareMock.mockReset();
	leaveShareMock.mockReset();
	outsideShareMock.mockReset();
	outsideShareMock.mockResolvedValue([MINE, OTHER]);
	assignMock.mockReset();
	remoteMock.mockReset();
	remoteMock.mockReturnValue(true);
	projectSetActiveMock.mockReset();
	projectSetActiveMock.mockResolvedValue(undefined);
	accessStatusMock.mockResolvedValue({ tier: 't1' });
	accessSharesListMock.mockResolvedValue([SHARE_VIEW]);
	__resetShareModeForTests();
	__resetSharedProjectsForTests();
	Object.defineProperty(window, 'location', {
		configurable: true,
		value: { ...realLocation, assign: assignMock },
	});
	useShellStore.setState({
		projects: [MINE, OTHER],
		activeProjectId: MINE.id,
		setActiveProject: realSetActive,
	});
});
afterEach(() => {
	cleanup();
	sessionStorage.clear();
	Object.defineProperty(window, 'location', { configurable: true, value: realLocation });
});

async function openChip() {
	const user = userEvent.setup();
	render(<TitleRow mac />);
	await user.click(screen.getByTestId('title-project-chip'));
	return user;
}

describe('project chip — Shared with you', () => {
	it('makes no access_shares_list call until the popover opens', async () => {
		render(<TitleRow mac />);
		await Promise.resolve();
		expect(accessSharesListMock).not.toHaveBeenCalled();
		expect(accessStatusMock).not.toHaveBeenCalled();
		await userEvent.setup().click(screen.getByTestId('title-project-chip'));
		await screen.findByTestId('title-shared-section');
		expect(accessSharesListMock).toHaveBeenCalledTimes(1);
	});

	it('lists shares with owner and role; clicking one calls openShare with that view', async () => {
		const user = await openChip();
		const section = await screen.findByTestId('title-shared-section');
		expect(section.textContent).toContain('Shared with you');
		expect(section.textContent).toContain('royalti-co');
		expect(section.textContent).toContain('ada · Reviewer');
		await user.click(screen.getByRole('button', { name: /royalti-co/ }));
		expect(openShareMock).toHaveBeenCalledWith(SHARE_VIEW);
	});

	it('is hidden when the session is not T1', async () => {
		accessStatusMock.mockResolvedValue({ tier: 't0' });
		await openChip();
		await waitFor(() => expect(accessStatusMock).toHaveBeenCalled());
		expect(screen.getByText('Switch project')).toBeTruthy();
		expect(screen.queryByTestId('title-shared-section')).toBeNull();
		expect(accessSharesListMock).not.toHaveBeenCalled();
	});

	it('is hidden when the list is empty', async () => {
		accessSharesListMock.mockResolvedValue([]);
		await openChip();
		await waitFor(() => expect(accessSharesListMock).toHaveBeenCalled());
		expect(screen.queryByTestId('title-shared-section')).toBeNull();
	});

	it('is hidden, with no error noise, when the call fails', async () => {
		accessSharesListMock.mockRejectedValue(new Error('boom'));
		await openChip();
		await waitFor(() => expect(accessSharesListMock).toHaveBeenCalled());
		expect(screen.queryByTestId('title-shared-section')).toBeNull();
		expect(screen.queryByRole('alert')).toBeNull();
		expect(screen.getByText('Label Ops', { selector: 'span.flex-1' })).toBeTruthy();
	});
});

// At runtime the broker narrows the store's `project_list` (loaded over the
// share transport) to the shared project alone.
const SHARED_ROW = mk('royalti-co', 'royalti-co', 0);

describe('project chip — inside a share', () => {
	beforeEach(() => {
		useShellStore.setState({ projects: [SHARED_ROW], activeProjectId: SHARED_ROW.id });
		setShareMode({
			projectKey: SHARE_VIEW.projectKey,
			projectId: SHARE_VIEW.projectId,
			projectName: SHARE_VIEW.projectName,
			ownerUsername: 'ada',
			role: 'reviewer',
			scope: 'project',
			artifactPath: null,
		});
	});

	it('labels the chip with the shared project and a shared-by marker', () => {
		render(<TitleRow mac />);
		const chip = screen.getByTestId('title-project-chip');
		expect(chip.textContent).toContain('royalti-co');
		expect(screen.getByTestId('title-project-chip-shared').textContent).toBe('shared by ada');
		expect(chip.getAttribute('data-shared')).toBe('true');
	});

	it('puts "Leave shared project" first and does not fetch shares', async () => {
		const user = await openChip();
		const leave = await screen.findByTestId('title-leave-shared');
		expect(leave.textContent).toContain('Leave shared project');
		expect(screen.queryByTestId('title-shared-section')).toBeNull();
		expect(accessSharesListMock).not.toHaveBeenCalled();
		await user.click(leave);
		expect(leaveShareMock).toHaveBeenCalledTimes(1);
	});

	it('fetches the real own-project list outside the share, only once opened', async () => {
		outsideShareMock.mockResolvedValue([MINE, OTHER]);
		render(<TitleRow mac />);
		await Promise.resolve();
		expect(outsideShareMock).not.toHaveBeenCalled();
		await userEvent.setup().click(screen.getByTestId('title-project-chip'));
		expect(await screen.findByRole('button', { name: /Catalog/ })).toBeTruthy();
		expect(outsideShareMock).toHaveBeenCalledWith('project_list', { includeArchived: true });
		expect(screen.getByText('Your projects')).toBeTruthy();
		expect(screen.queryByText(/Loading projects/)).toBeNull();
	});

	it('picking an own project leaves the share first, then switches', async () => {
		outsideShareMock.mockResolvedValue([MINE, OTHER]);
		const order: string[] = [];
		projectSetActiveMock.mockImplementation(async (id: string) => {
			order.push(`switch:${id}:share=${currentShare() ? 'on' : 'off'}`);
		});
		const user = await openChip();
		await user.click(await screen.findByRole('button', { name: /Catalog/ }));
		await waitFor(() => expect(assignMock).toHaveBeenCalledWith('/'));
		expect(order).toEqual(['switch:catalog:share=off']);
		expect(currentShare()).toBeNull();
	});

	it('shows a loading state, then only Leave, when the own-project fetch fails', async () => {
		let reject!: (e: Error) => void;
		outsideShareMock.mockReturnValue(
			new Promise((_, r) => {
				reject = r;
			})
		);
		await openChip();
		expect(await screen.findByTestId('title-own-projects-loading')).toBeTruthy();
		reject(new Error('boom'));
		await waitFor(() => expect(screen.queryByTestId('title-own-projects-loading')).toBeNull());
		expect(screen.queryByText('Your projects')).toBeNull();
		expect(screen.queryByText(/Loading/)).toBeNull();
		expect(screen.getByTestId('title-leave-shared')).toBeTruthy();
	});
});

describe('project chip — desktop is unchanged', () => {
	it('sends no access_status / access_shares_list and renders no shared section', async () => {
		remoteMock.mockReturnValue(false);
		await openChip();
		expect(await screen.findByText('Switch project')).toBeTruthy();
		await Promise.resolve();
		expect(accessStatusMock).not.toHaveBeenCalled();
		expect(accessSharesListMock).not.toHaveBeenCalled();
		expect(outsideShareMock).not.toHaveBeenCalled();
		expect(screen.queryByTestId('title-shared-section')).toBeNull();
		expect(screen.getByText('New project…')).toBeTruthy();
	});
});

describe('project chip — own project sharing the shared project id', () => {
	beforeEach(() => {
		useShellStore.setState({
			projects: [mk('royalti-co', 'royalti-co', 0)],
			activeProjectId: 'royalti-co',
		});
		setShareMode({
			projectKey: SHARE_VIEW.projectKey,
			projectId: 'royalti-co',
			projectName: 'royalti-co',
			ownerUsername: 'ada',
			role: 'reviewer',
			scope: 'project',
			artifactPath: null,
		});
	});

	it('sends project_set_active without the share, then reloads to /', async () => {
		const own = [mk('label-ops', 'Label Ops', 0), mk('royalti-co', 'Mine royalti', 1)];
		outsideShareMock.mockResolvedValue(own);
		const seen: string[] = [];
		projectSetActiveMock.mockImplementation(async (id: string) => {
			seen.push(`${id}:share=${currentShare() ? 'on' : 'off'}`);
		});
		const user = await openChip();
		await user.click(await screen.findByRole('button', { name: /Mine royalti/ }));
		await waitFor(() => expect(assignMock).toHaveBeenCalledWith('/'));
		expect(seen).toEqual(['royalti-co:share=off']);
		expect(projectSetActiveMock.mock.invocationCallOrder[0]).toBeLessThan(
			assignMock.mock.invocationCallOrder[0]!
		);
	});

	it('hides New project… and Manage projects… inside a share', async () => {
		await openChip();
		await screen.findByTestId('title-leave-shared');
		expect(screen.queryByText('New project…')).toBeNull();
		expect(screen.queryByText('Manage projects…')).toBeNull();
	});
});
