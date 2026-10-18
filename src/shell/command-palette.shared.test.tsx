// The ⌘P projects palette inside a share: the store's `projects` is the
// broker-narrowed list (shared project only), so the palette must fetch the
// real own-project list outside the share — never show a placeholder that
// cannot resolve ("No projects loaded.") — and picking one leaves first.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const outsideShareMock = vi.hoisted(() => vi.fn());
vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	invokeOutsideShare: outsideShareMock,
	isRemoteWebSession: remoteMock,
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	projectSetActive: projectSetActiveMock,
}));

const remoteMock = vi.hoisted(() => vi.fn());
const projectSetActiveMock = vi.hoisted(() => vi.fn());
const accessSharesListMock = vi.hoisted(() => vi.fn());
vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	accessStatus: vi.fn().mockResolvedValue({ tier: 't1' }),
	accessSharesList: accessSharesListMock,
}));

const leaveShareMock = vi.hoisted(() => vi.fn());
vi.mock('./people/shared-with-you', async (orig) => ({
	...(await orig<typeof import('./people/shared-with-you')>()),
	leaveShare: leaveShareMock,
}));

import { accessStatus } from '@/lib/access/client';
import { useShellStore } from '@/lib/shell/shell-store';
import type { Project } from '@/lib/tauri-cmd';
import { __resetShareModeForTests, currentShare, setShareMode } from '@/lib/transport';
import { CommandPalette } from './command-palette';

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

const SHARED_ROW = mk('royalti-co', 'royalti-co', 0);
const MINE = mk('label-ops', 'Label Ops', 0);
const OTHER = mk('catalog', 'Catalog', 1);

const assignMock = vi.fn();
const accessStatusSpy = vi.mocked(accessStatus);
const realSetActive = useShellStore.getState().setActiveProject;
const realLocation = window.location;

beforeEach(() => {
	// cmdk measures its list with ResizeObserver / scrollIntoView (absent in jsdom).
	vi.stubGlobal(
		'ResizeObserver',
		class {
			observe() {}
			unobserve() {}
			disconnect() {}
		}
	);
	Element.prototype.scrollIntoView = vi.fn();
	outsideShareMock.mockReset();
	accessSharesListMock.mockReset();
	assignMock.mockReset();
	remoteMock.mockReset();
	remoteMock.mockReturnValue(true);
	projectSetActiveMock.mockReset();
	projectSetActiveMock.mockResolvedValue(undefined);
	accessStatusSpy.mockClear();
	__resetShareModeForTests();
	setShareMode({
		projectKey: '11111111-1111-1111-1111-111111111111/royalti-co',
		projectId: 'royalti-co',
		projectName: 'royalti-co',
		ownerUsername: 'ada',
		role: 'reviewer',
		scope: 'project',
		artifactPath: null,
	});
	Object.defineProperty(window, 'location', {
		configurable: true,
		value: { ...realLocation, assign: assignMock },
	});
	useShellStore.setState({
		projects: [SHARED_ROW],
		activeProjectId: SHARED_ROW.id,
		setActiveProject: realSetActive,
	});
});
afterEach(() => {
	vi.unstubAllGlobals();
	cleanup();
	sessionStorage.clear();
	Object.defineProperty(window, 'location', { configurable: true, value: realLocation });
});

function renderPalette() {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={client}>
			<CommandPalette open mode="projects" onOpenChange={() => {}} />
		</QueryClientProvider>
	);
}

describe('⌘P projects palette — inside a share', () => {
	it('lists the real own projects fetched outside the share, with Leave first', async () => {
		outsideShareMock.mockResolvedValue([MINE, OTHER]);
		renderPalette();
		expect(await screen.findByText('Catalog')).toBeTruthy();
		expect(outsideShareMock).toHaveBeenCalledWith('project_list', { includeArchived: true });
		expect(screen.getByText('Leave shared project')).toBeTruthy();
		expect(screen.queryByText('No projects loaded.')).toBeNull();
	});

	it('picking an own project leaves the share before switching', async () => {
		outsideShareMock.mockResolvedValue([MINE, OTHER]);
		const order: string[] = [];
		projectSetActiveMock.mockImplementation(async (id: string) => {
			order.push(`switch:${id}:share=${currentShare() ? 'on' : 'off'}`);
		});
		renderPalette();
		await userEvent.setup().click(await screen.findByText('Catalog'));
		await waitFor(() => expect(assignMock).toHaveBeenCalledWith('/'));
		expect(order).toEqual(['switch:catalog:share=off']);
	});

	it('drops the own-projects group (no stuck placeholder) when the fetch fails', async () => {
		outsideShareMock.mockRejectedValue(new Error('boom'));
		renderPalette();
		await waitFor(() => expect(outsideShareMock).toHaveBeenCalled());
		await waitFor(() => expect(screen.queryByText(/Loading your projects/)).toBeNull());
		expect(screen.queryByText('Your projects')).toBeNull();
		expect(screen.queryByText('No projects loaded.')).toBeNull();
		expect(screen.getByText('Leave shared project')).toBeTruthy();
	});
});

describe('⌘P projects palette — extra share/desktop rules', () => {
	it('picking an own project with the shared id sends project_set_active outside the share, then reloads', async () => {
		outsideShareMock.mockResolvedValue([MINE, mk('royalti-co', 'Mine royalti', 1)]);
		const seen: string[] = [];
		projectSetActiveMock.mockImplementation(async (id: string) => {
			seen.push(`${id}:share=${currentShare() ? 'on' : 'off'}`);
		});
		renderPalette();
		await userEvent.setup().click(await screen.findByText('Mine royalti'));
		await waitFor(() => expect(assignMock).toHaveBeenCalledWith('/'));
		expect(seen).toEqual(['royalti-co:share=off']);
	});

	it('hides "Open Projects settings…" inside a share', async () => {
		outsideShareMock.mockResolvedValue([MINE]);
		renderPalette();
		await screen.findByText('Leave shared project');
		expect(screen.queryByText('Open Projects settings…')).toBeNull();
	});

	it('desktop: no access calls, no shared section, Manage kept', async () => {
		remoteMock.mockReturnValue(false);
		setShareMode(null);
		useShellStore.setState({ projects: [MINE, OTHER], activeProjectId: MINE.id });
		renderPalette();
		expect(await screen.findByText('Open Projects settings…')).toBeTruthy();
		await Promise.resolve();
		expect(accessStatusSpy).not.toHaveBeenCalled();
		expect(accessSharesListMock).not.toHaveBeenCalled();
		expect(screen.queryByText('Shared with you')).toBeNull();
	});
});
