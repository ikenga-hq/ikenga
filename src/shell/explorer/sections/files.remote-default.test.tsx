import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { AccessStatus } from '@/lib/access/client';
import { useFilesStore } from '@/lib/shell/files-store';
import { FilesSection } from './files';

const h = vi.hoisted(() => ({
	remote: false,
	home: '/home/ik-ada' as string | null,
	rootsList: ['/home/ik-ada'] as string[],
	shellState: {
		activeProject: { id: 'default', root_path: null as string | null, extra_roots: [] as string[] },
		activeProjectId: 'default',
	},
	accessStatus: {
		tier: 't1',
		store: 'ok',
		principal: { principalId: 'u1', username: 'ik-ada', isAdmin: false },
		credential: { via: 'session', deviceId: null, tier: 'full' },
		caps: ['files'],
		adminStrength: false,
		publicUrl: null,
		sharingEnabled: true,
		share: null,
	} as AccessStatus,
}));

vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isRemoteWebSession: () => h.remote,
}));

vi.mock('@/lib/transport/path-shim', async (orig) => ({
	...(await orig<typeof import('@/lib/transport/path-shim')>()),
	homeDir: vi.fn(async () => h.home ?? '~'),
}));

vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	isRemoteWebSession: () => h.remote,
	fsRootsList: vi.fn(async () => h.rootsList),
	fsList: vi.fn(async (dir: string) => [
		{ name: 'deliverables', path: `${dir}/deliverables`, isDir: true },
		{ name: 'notes.txt', path: `${dir}/notes.txt`, isDir: false },
	]),
	fsSearch: vi.fn(),
	fsRename: vi.fn(),
	fsTrash: vi.fn(),
}));

vi.mock('@/lib/access/client', async (orig) => ({
	...(await orig<typeof import('@/lib/access/client')>()),
	accessStatus: vi.fn(async () => h.accessStatus),
}));

vi.mock('@/lib/shell/shell-store', () => ({
	useShellStore: Object.assign((sel: (s: typeof h.shellState) => unknown) => sel(h.shellState), {
		getState: () => h.shellState,
		subscribe: () => () => {},
	}),
}));

vi.mock('@/lib/shell/use-git-status', () => ({
	useGitStatus: () => ({ data: undefined }),
}));

global.ResizeObserver = class ResizeObserver {
	observe() {}
	unobserve() {}
	disconnect() {}
};

function renderFiles(projectId = 'default') {
	const qc = new QueryClient({
		defaultOptions: {
			queries: { retry: false, gcTime: 0 },
		},
	});
	return render(
		<QueryClientProvider client={qc}>
			<FilesSection projectId={projectId} />
		</QueryClientProvider>
	);
}

describe('FilesSection remote default project home fallback', () => {
	beforeEach(() => {
		vi.clearAllMocks();
		useFilesStore.setState({
			hydrated: true,
			expandedRoot: null,
			expanded: new Set(),
			queries: {},
			showHidden: false,
			showIgnored: false,
		});
		h.remote = false;
		h.home = '/home/ik-ada';
		h.rootsList = ['/home/ik-ada'];
		h.shellState.activeProject = { id: 'default', root_path: null, extra_roots: [] };
		h.shellState.activeProjectId = 'default';
	});

	afterEach(() => {
		cleanup();
	});

	it('remote + default + no root → home root', async () => {
		h.remote = true;
		h.shellState.activeProject = { id: 'default', root_path: null, extra_roots: [] };
		h.shellState.activeProjectId = 'default';
		h.home = '/home/ik-ada';
		h.rootsList = ['/home/ik-ada'];

		renderFiles('default');

		// The root accordion header shows the folder name (ik-ada)
		await waitFor(() => {
			expect(screen.getByText('ik-ada')).toBeTruthy();
		});

		// And its files (deliverables) are listed
		await waitFor(() => {
			expect(screen.getByText('deliverables')).toBeTruthy();
			expect(screen.getByText('notes.txt')).toBeTruthy();
		});
	});

	it('desktop → unchanged (shows no project open)', async () => {
		h.remote = false;
		h.shellState.activeProject = { id: 'default', root_path: null, extra_roots: [] };
		h.shellState.activeProjectId = 'default';

		renderFiles('default');

		await waitFor(() => {
			expect(screen.getByText('No project open')).toBeTruthy();
			expect(screen.getByText('Open a folder…')).toBeTruthy();
		});
		expect(screen.queryByText('ik-ada')).toBeNull();
	});

	it('non-default → unchanged (uses project root)', async () => {
		h.remote = true;
		h.shellState.activeProject = { id: 'other-proj', root_path: '/srv/project', extra_roots: [] };
		h.shellState.activeProjectId = 'other-proj';

		renderFiles('other-proj');

		await waitFor(() => {
			expect(screen.getByText('project')).toBeTruthy();
		});
		expect(screen.queryByText('ik-ada')).toBeNull();
	});

	it('non-default with no root → unchanged (shows no project open)', async () => {
		h.remote = true;
		h.shellState.activeProject = { id: 'empty-proj', root_path: null, extra_roots: [] };
		h.shellState.activeProjectId = 'empty-proj';

		renderFiles('empty-proj');

		await waitFor(() => {
			expect(screen.getByText('No project open')).toBeTruthy();
		});
		expect(screen.queryByText('ik-ada')).toBeNull();
	});

	it('home unknown → reason shown', async () => {
		h.remote = true;
		h.shellState.activeProject = { id: 'default', root_path: null, extra_roots: [] };
		h.shellState.activeProjectId = 'default';
		h.home = '~';
		h.rootsList = [];

		renderFiles('default');

		await waitFor(() => {
			expect(screen.getByText('Home folder unavailable')).toBeTruthy();
			expect(screen.getByText('Your home directory could not be determined for this session.')).toBeTruthy();
		});
		expect(screen.queryByText('No project open')).toBeNull();
	});

	it('home not in allowlist → access level explanation shown', async () => {
		h.remote = true;
		h.shellState.activeProject = { id: 'default', root_path: null, extra_roots: [] };
		h.shellState.activeProjectId = 'default';
		h.home = '/home/ik-ada';
		h.rootsList = []; // empty allowlist, e.g. T0 or tier restriction

		renderFiles('default');

		await waitFor(() => {
			expect(screen.getByText('Not available for your access level')).toBeTruthy();
			expect(screen.getByText('Ask an admin of this server to add a folder for you.')).toBeTruthy();
		});
	});
});
