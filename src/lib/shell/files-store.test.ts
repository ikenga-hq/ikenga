import { beforeEach, describe, expect, it, vi } from 'vitest';

// Stub layout-state so the store doesn't try to hit SQLite/localStorage in
// tests. We don't care about persistence here — the visibility-flag behavior
// can be exercised entirely in-memory.
vi.mock('@/lib/layout-state', () => ({
	loadLayoutState: vi.fn(async (_key: string, fallback: unknown) => fallback),
	saveLayoutState: vi.fn(async () => {}),
	// Run synchronously in tests so we don't have to await the debounce.
	// Return a callable with a `flush` no-op to mimic the real signature.
	debounce: <A extends unknown[]>(fn: (...args: A) => void) => {
		const wrapper = ((...args: A) => fn(...args)) as ((...args: A) => void) & {
			flush: () => void;
		};
		wrapper.flush = () => {};
		return wrapper;
	},
}));

import { useFilesStore } from './files-store';
import { useShellStore } from './shell-store';

beforeEach(() => {
	// Reset to defaults — tests shouldn't leak state into each other.
	useShellStore.setState({
		activeProjectId: 'default',
	});
	useFilesStore.setState({
		activeProjectId: 'default',
		byProject: {},
		expanded: new Set<string>(),
		selectedPath: null,
		scrollTop: 0,
		showHidden: false,
		showIgnored: false,
		queries: {},
		expandedRoot: null,
		hydrated: false,
	});
});

describe('files-store visibility flags', () => {
	it('defaults both visibility flags to false', () => {
		const s = useFilesStore.getState();
		expect(s.showHidden).toBe(false);
		expect(s.showIgnored).toBe(false);
	});

	it('toggleShowHidden flips the flag', () => {
		const { toggleShowHidden } = useFilesStore.getState();
		toggleShowHidden();
		expect(useFilesStore.getState().showHidden).toBe(true);
		toggleShowHidden();
		expect(useFilesStore.getState().showHidden).toBe(false);
	});

	it('toggleShowIgnored flips the flag independently of showHidden', () => {
		const { toggleShowIgnored } = useFilesStore.getState();
		toggleShowIgnored();
		expect(useFilesStore.getState().showIgnored).toBe(true);
		expect(useFilesStore.getState().showHidden).toBe(false);
	});

	it('setShowHidden is idempotent — no-op if value matches current state', () => {
		const { setShowHidden } = useFilesStore.getState();
		setShowHidden(false); // already false
		expect(useFilesStore.getState().showHidden).toBe(false);
		setShowHidden(true);
		expect(useFilesStore.getState().showHidden).toBe(true);
		setShowHidden(true); // already true
		expect(useFilesStore.getState().showHidden).toBe(true);
	});

	it('hydrate populates flags from persisted snapshot', async () => {
		const layoutState = await import('@/lib/layout-state');
		(layoutState.loadLayoutState as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
			expanded: ['/a'],
			selectedPath: '/a/file.ts',
			scrollTop: 42,
			showHidden: true,
			showIgnored: true,
		});

		await useFilesStore.getState().hydrate();
		const s = useFilesStore.getState();
		expect(s.showHidden).toBe(true);
		expect(s.showIgnored).toBe(true);
		expect(s.expanded.has('/a')).toBe(true);
		expect(s.scrollTop).toBe(42);
		expect(s.hydrated).toBe(true);
	});

	it('hydrate falls back to defaults when persisted snapshot lacks flags (older format)', async () => {
		const layoutState = await import('@/lib/layout-state');
		// Simulate a pre-v2 record without showHidden/showIgnored.
		(layoutState.loadLayoutState as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
			expanded: [],
			selectedPath: null,
			scrollTop: 0,
		});

		await useFilesStore.getState().hydrate();
		const s = useFilesStore.getState();
		expect(s.showHidden).toBe(false);
		expect(s.showIgnored).toBe(false);
	});
});

describe('files-store project isolation and synchronization (WP-05)', () => {
	it('switching projects A -> B -> A restores expanded set and selection byte-for-byte (DoD 3)', () => {
		const store = useFilesStore.getState();

		// Start on project-A
		store.setActiveProjectId('project-a');
		store.expand('/a/folder1');
		store.expand('/a/folder2');
		store.setSelected('/a/folder1/file.ts');
		store.setScrollTop(150);
		store.setShowHidden(true);

		expect([...useFilesStore.getState().expanded].sort()).toEqual(['/a/folder1', '/a/folder2']);
		expect(useFilesStore.getState().selectedPath).toBe('/a/folder1/file.ts');
		expect(useFilesStore.getState().scrollTop).toBe(150);
		expect(useFilesStore.getState().showHidden).toBe(true);

		// Switch to project-B: fresh default state
		store.setActiveProjectId('project-b');
		expect(useFilesStore.getState().activeProjectId).toBe('project-b');
		expect([...useFilesStore.getState().expanded]).toEqual([]);
		expect(useFilesStore.getState().selectedPath).toBeNull();
		expect(useFilesStore.getState().scrollTop).toBe(0);
		expect(useFilesStore.getState().showHidden).toBe(false);

		// Modify project-B state
		useFilesStore.getState().expand('/b/different-dir');
		useFilesStore.getState().setSelected('/b/different-dir/main.go');
		useFilesStore.getState().setScrollTop(300);

		expect([...useFilesStore.getState().expanded]).toEqual(['/b/different-dir']);
		expect(useFilesStore.getState().selectedPath).toBe('/b/different-dir/main.go');

		// Switch back to project-A: restores exactly A's state
		useFilesStore.getState().setActiveProjectId('project-a');
		expect(useFilesStore.getState().activeProjectId).toBe('project-a');
		expect([...useFilesStore.getState().expanded].sort()).toEqual(['/a/folder1', '/a/folder2']);
		expect(useFilesStore.getState().selectedPath).toBe('/a/folder1/file.ts');
		expect(useFilesStore.getState().scrollTop).toBe(150);
		expect(useFilesStore.getState().showHidden).toBe(true);

		// Switch back to project-B: restores exactly B's state
		useFilesStore.getState().setActiveProjectId('project-b');
		expect(useFilesStore.getState().activeProjectId).toBe('project-b');
		expect([...useFilesStore.getState().expanded]).toEqual(['/b/different-dir']);
		expect(useFilesStore.getState().selectedPath).toBe('/b/different-dir/main.go');
		expect(useFilesStore.getState().scrollTop).toBe(300);
	});

	it('reflects project switch from useShellStore in the same tick (DoD 2)', () => {
		useFilesStore.getState().setActiveProjectId('proj-alpha');
		useFilesStore.getState().expand('/alpha/pkg');

		// Flips activeProjectId in useShellStore
		useShellStore.setState({ activeProjectId: 'proj-beta' });

		// Synchronous check in the same tick
		expect(useFilesStore.getState().activeProjectId).toBe('proj-beta');
		expect([...useFilesStore.getState().expanded]).toEqual([]);

		// Switch back via useShellStore
		useShellStore.setState({ activeProjectId: 'proj-alpha' });
		expect(useFilesStore.getState().activeProjectId).toBe('proj-alpha');
		expect([...useFilesStore.getState().expanded]).toEqual(['/alpha/pkg']);
	});

	it('migrates legacy v1 unkeyed layout data into default project on hydrate', async () => {
		const layoutState = await import('@/lib/layout-state');
		// Legacy snapshot with root-level expanded/selectedPath
		(layoutState.loadLayoutState as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
			expanded: ['/legacy/src'],
			selectedPath: '/legacy/src/index.ts',
			scrollTop: 75,
			showHidden: true,
			showIgnored: false,
			expandedRoot: '/legacy',
		});

		useShellStore.setState({ activeProjectId: 'default' });
		useFilesStore.setState({ hydrated: false, byProject: {} });

		await useFilesStore.getState().hydrate();

		const s = useFilesStore.getState();
		expect(s.hydrated).toBe(true);
		expect(s.activeProjectId).toBe('default');
		expect([...s.expanded]).toEqual(['/legacy/src']);
		expect(s.selectedPath).toBe('/legacy/src/index.ts');
		expect(s.scrollTop).toBe(75);
		expect(s.showHidden).toBe(true);
		expect(s.byProject['default']).toBeDefined();
		expect(s.byProject['default']?.expanded).toEqual(['/legacy/src']);
	});
});
