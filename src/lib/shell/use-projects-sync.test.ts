import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { useFilesStore } from '@/lib/shell/files-store';
import { useShellStore } from '@/lib/shell/shell-store';
import type { Project } from '@/lib/tauri-cmd';

let listenerCallback: (() => void) | null = null;
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
	projectGetActive: vi.fn(async () => mockActiveProject),
	projectList: vi.fn(async () => mockProjects),
}));

import { useProjectsSync } from './use-projects-sync';

describe('useProjectsSync — WP-05 / WP-21 bridge integration (DoD 4)', () => {
	beforeEach(() => {
		listenerCallback = null;
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
