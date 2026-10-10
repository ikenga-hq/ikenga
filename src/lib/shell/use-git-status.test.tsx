// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const sidecarMock = vi.hoisted(() => vi.fn());
const isRemoteWebSessionMock = vi.hoisted(() => vi.fn(() => false));
const gitStatusMock = vi.hoisted(() => vi.fn());
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	pkgSidecarCall: sidecarMock,
	isRemoteWebSession: isRemoteWebSessionMock,
	gitStatus: gitStatusMock,
}));

import { useShellStore } from '@/lib/shell/shell-store';
import type { Project } from '@/lib/tauri-cmd';
import { useGitStatus } from './use-git-status';

const PROJECT = {
	id: 'label-ops',
	display_name: 'Label Ops',
	root_path: '/home/e2e/label-ops',
} as Project;

function wrapper({ children }: { children: ReactNode }) {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}

beforeEach(() => {
	sidecarMock.mockReset();
	gitStatusMock.mockReset();
	isRemoteWebSessionMock.mockReset();
	isRemoteWebSessionMock.mockReturnValue(true);
	useShellStore.setState({ projects: [PROJECT], activeProjectId: PROJECT.id });
});
afterEach(cleanup);

describe('useGitStatus — remote session', () => {
	it('builds the Explorer badges from git_status and never calls the sidecar', async () => {
		gitStatusMock.mockResolvedValue({
			branch: 'main',
			detached: false,
			ahead: 0,
			behind: 0,
			staged: [{ path: 'src/a.ts' }],
			unstaged: [{ path: 'src/b.ts' }],
			untracked: [{ path: 'notes.md' }],
			conflicted: [{ path: 'src/c.ts' }],
			modified: 4,
		});
		const { result } = renderHook(() => useGitStatus(), { wrapper });
		await waitFor(() => expect(result.current.data?.files.size).toBe(4));
		const { files, dirtyFolders } = result.current.data!;
		expect(files.get(`${PROJECT.root_path}/src/a.ts`)).toBe('added');
		expect(files.get(`${PROJECT.root_path}/src/b.ts`)).toBe('modified');
		expect(files.get(`${PROJECT.root_path}/notes.md`)).toBe('untracked');
		expect(files.get(`${PROJECT.root_path}/src/c.ts`)).toBe('conflicted');
		expect(dirtyFolders.has(`${PROJECT.root_path}/src`)).toBe(true);
		expect(gitStatusMock).toHaveBeenCalledWith({ root: PROJECT.root_path, projectId: PROJECT.id });
		expect(sidecarMock).not.toHaveBeenCalled();
	});

	it('shows nothing (no badges) when git_status is null or fails', async () => {
		gitStatusMock.mockResolvedValueOnce(null);
		const a = renderHook(() => useGitStatus(), { wrapper });
		await waitFor(() => expect(a.result.current.isSuccess).toBe(true));
		expect(a.result.current.data?.files.size).toBe(0);
		cleanup();

		gitStatusMock.mockRejectedValueOnce(new Error('git status timed out'));
		const b = renderHook(() => useGitStatus(), { wrapper });
		await waitFor(() => expect(b.result.current.isSuccess).toBe(true));
		expect(b.result.current.data?.files.size).toBe(0);
		expect(b.result.current.data?.dirtyFolders.size).toBe(0);
	});
});
