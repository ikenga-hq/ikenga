// roots-body — verifies the shell-store interactions the wizard relies on.

import { beforeEach, describe, expect, it } from 'vitest';

import { useShellStore } from '@/lib/shell/shell-store';

import { mirrorProjectsToFileRoots } from './roots-body';

beforeEach(() => {
	// Reset projectExtraRoots and activeProject for clean state
	useShellStore.setState({
		activeProjectId: 'default',
		projectExtraRoots: {},
		carriedRoots: [],
		activeProject: { id: 'default', root_path: null, extra_roots: [] },
	});
});

describe('roots step — store interactions', () => {
	it('starts with empty extra_roots by default', () => {
		const s = useShellStore.getState();
		expect(s.activeProject.extra_roots).toEqual([]);
	});

	it('appends a custom root via setProjectExtraRoots', () => {
		const s = useShellStore.getState();
		s.setProjectExtraRoots('default', ['~/custom/path']);
		expect(useShellStore.getState().activeProject.extra_roots).toEqual(['~/custom/path']);
	});

	it('deduplicates when setting roots with duplicates', () => {
		const s = useShellStore.getState();
		s.setProjectExtraRoots('default', ['~/dup', '~/dup', '~/other']);
		expect(useShellStore.getState().activeProject.extra_roots).toEqual(['~/dup', '~/other']);
	});

	it('allows removing an extra root', () => {
		const s = useShellStore.getState();
		s.setProjectExtraRoots('default', ['~/root-1', '~/root-2']);
		expect(useShellStore.getState().activeProject.extra_roots).toHaveLength(2);

		s.setProjectExtraRoots('default', ['~/root-1']);
		expect(useShellStore.getState().activeProject.extra_roots).toEqual(['~/root-1']);
	});
});

describe('roots step — Continue mirrors projects into extra_roots', () => {
	it('copies project paths into extra_roots, skipping ones already present', () => {
		useShellStore.getState().setProjectExtraRoots('default', ['~/Code/existing']);

		mirrorProjectsToFileRoots(['~/Code/existing', '~/Code/brand-new']);

		const after = useShellStore.getState().activeProject.extra_roots;
		expect(after).toContain('~/Code/existing');
		expect(after).toContain('~/Code/brand-new');
		expect(after.filter((p) => p === '~/Code/existing')).toHaveLength(1);
	});

	it('is a no-op when projects list is empty', () => {
		useShellStore.getState().setProjectExtraRoots('default', ['~/keep/this']);
		mirrorProjectsToFileRoots([]);
		expect(useShellStore.getState().activeProject.extra_roots).toEqual(['~/keep/this']);
	});
});
