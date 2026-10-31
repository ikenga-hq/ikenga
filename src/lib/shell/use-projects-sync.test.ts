import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { queryClient } from '@/lib/query-client';
import { useFilesStore } from '@/lib/shell/files-store';
import { useShellStore } from '@/lib/shell/shell-store';
import type { Project } from '@/lib/tauri-cmd';

let listenerCallback: (() => void) | null = null;
let listChangedCallback: ((p: { id: string; kind: string }) => void) | null = null;
let mockActiveProject: Project = {
	id: 'proj-alpha',
	display_name: 'Alpha',
	root_path: '/path/to/alpha',
	icon: null,
	color: null,
	description: null,
	position: 0,
	is_default: false,
	created_at: 0,
	archived_at: null,
};
let mockProjects: Project[] = [mockActiveProject];

vi.mock('@/lib/tauri-cmd', () => ({
	projectListenActiveChanged: vi.fn((cb: () => void) => {
		listenerCallback = cb;
		return Promise.resolve(() => {
			listenerCallback = null;
		});
	}),
	projectListenListChanged: vi.fn((cb: (p: { id: string; kind: string }) => void) => {
		listChangedCallback = cb;
		return Promise.resolve(() => {
			listChangedCallback = null;
		});
	}),
	projectGetActive: vi.fn(async () => mockActiveProject),
	projectList: vi.fn(async () => mockProjects),
}));

import { useProjectsSync } from './use-projects-sync';

describe('useProjectsSync — WP-05 / WP-21 bridge integration (DoD 4)', () => {
	beforeEach(() => {
		listenerCallback = null;
		listChangedCallback = null;
		mockActiveProject = {
			id: 'proj-alpha',
			display_name: 'Alpha',
			root_path: '/path/to/alpha',
			icon: null,
			color: null,
			description: null,
			position: 0,
			is_default: false,
			created_at: 0,
			archived_at: null,
		};
		mockProjects = [
			mockActiveProject,
			{
				id: 'proj-beta',
				display_name: 'Beta',
				root_path: '/path/to/beta',
				icon: null,
				color: null,
				description: null,
				position: 1,
				is_default: false,
				created_at: 0,
				archived_at: null,
			},
		];

		useShellStore.setState({
			activeProjectId: 'proj-alpha',
			projects: mockProjects,
			projectExtraRoots: {},
			carriedRoots: [],
			activeProject: { id: 'proj-alpha', root_path: '/path/to/alpha', extra_roots: [] },
		});

		useFilesStore.setState({
			activeProjectId: 'proj-alpha',
			byProject: {},
			expanded: new Set<string>(['/path/to/alpha/src']),
			selectedPath: '/path/to/alpha/src/index.ts',
			scrollTop: 100,
		});
	});

	it('listener triggers on Rust projects:active-changed and flips files-store tree (DoD 4)', async () => {
		const { unmount } = renderHook(() => useProjectsSync());

		// Wait for listener registration
		await vi.waitFor(() => {
			expect(listenerCallback).not.toBeNull();
		});

		// Simulate Rust-side project switch (e.g. `iyke project switch proj-beta`)
		mockActiveProject = mockProjects[1]!;

		// Fire event from Rust
		await act(async () => {
			listenerCallback!();
		});

		// Shell store updated
		expect(useShellStore.getState().activeProjectId).toBe('proj-beta');
		expect(useShellStore.getState().activeProject.root_path).toBe('/path/to/beta');

		// Files store flipped to proj-beta's tree in the same tick
		expect(useFilesStore.getState().activeProjectId).toBe('proj-beta');
		expect([...useFilesStore.getState().expanded]).toEqual([]);
		expect(useFilesStore.getState().selectedPath).toBeNull();

		// Switch back to proj-alpha
		mockActiveProject = mockProjects[0]!;
		await act(async () => {
			listenerCallback!();
		});

		// Proj-alpha's tree state is restored
		expect(useFilesStore.getState().activeProjectId).toBe('proj-alpha');
		expect([...useFilesStore.getState().expanded]).toEqual(['/path/to/alpha/src']);
		expect(useFilesStore.getState().selectedPath).toBe('/path/to/alpha/src/index.ts');
		expect(useFilesStore.getState().scrollTop).toBe(100);

		unmount();
	});
});

describe('useProjectsSync — projects:list-changed (G-EVENT)', () => {
	const row = (id: string, name: string, position: number): Project => ({
		id,
		display_name: name,
		root_path: `/path/to/${id}`,
		icon: null,
		color: null,
		description: null,
		position,
		is_default: false,
		created_at: 0,
		archived_at: null,
	});

	beforeEach(() => {
		listenerCallback = null;
		listChangedCallback = null;
		mockActiveProject = row('proj-alpha', 'Alpha', 0);
		mockProjects = [mockActiveProject];
		useShellStore.setState({
			activeProjectId: 'proj-alpha',
			projects: mockProjects,
			projectExtraRoots: {},
			carriedRoots: [],
			activeProject: { id: 'proj-alpha', root_path: '/path/to/alpha', extra_roots: [] },
		});
	});

	it('a created project appears in the store without a reload, and project-scoped queries are not invalidated', async () => {
		const invalidate = vi.spyOn(queryClient, 'invalidateQueries');
		const { unmount } = renderHook(() => useProjectsSync());
		await vi.waitFor(() => {
			expect(listChangedCallback).not.toBeNull();
		});
		expect(useShellStore.getState().projects.map((p) => p.id)).toEqual(['proj-alpha']);

		// Registered out of band (agent via the bridge): the row exists in
		// Rust, the store has not been told.
		mockProjects = [mockActiveProject, row('proj-gamma', 'Gamma', 1)];
		await act(async () => {
			listChangedCallback!({ id: 'proj-gamma', kind: 'created' });
		});

		await vi.waitFor(() => {
			expect(useShellStore.getState().projects.map((p) => p.id)).toEqual([
				'proj-alpha',
				'proj-gamma',
			]);
		});
		// The active project did not change, so no project-scoped churn.
		expect(useShellStore.getState().activeProjectId).toBe('proj-alpha');
		expect(invalidate).not.toHaveBeenCalled();

		invalidate.mockRestore();
		unmount();
	});

	it('calls refreshProjects once per event, for created, updated and archived alike', async () => {
		const refresh = vi.fn(async () => {});
		useShellStore.setState({ refreshProjects: refresh });
		const { unmount } = renderHook(() => useProjectsSync());
		await vi.waitFor(() => {
			expect(listChangedCallback).not.toBeNull();
		});

		for (const [i, kind] of (['created', 'updated', 'archived'] as const).entries()) {
			await act(async () => {
				listChangedCallback!({ id: 'proj-alpha', kind });
			});
			expect(refresh).toHaveBeenCalledTimes(i + 1);
		}
		unmount();
	});

	it('a switch still invalidates project-scoped queries (the other handler is untouched)', async () => {
		const invalidate = vi.spyOn(queryClient, 'invalidateQueries');
		const { unmount } = renderHook(() => useProjectsSync());
		await vi.waitFor(() => {
			expect(listenerCallback).not.toBeNull();
		});
		await act(async () => {
			listenerCallback!();
		});
		expect(invalidate).toHaveBeenCalledWith({ queryKey: ['project-scoped'] });
		invalidate.mockRestore();
		unmount();
	});

	it('unsubscribes both listeners on unmount', async () => {
		const { unmount } = renderHook(() => useProjectsSync());
		await vi.waitFor(() => {
			expect(listenerCallback).not.toBeNull();
			expect(listChangedCallback).not.toBeNull();
		});
		unmount();
		expect(listenerCallback).toBeNull();
		expect(listChangedCallback).toBeNull();
	});
});
