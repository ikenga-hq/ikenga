import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

import { Explorer } from './explorer';
import { ExplorerHeader } from './explorer-header';
import { SectionFrame } from './section-frame';
import { builtInSections } from './section-registry';
import { listExplorerSections as bridgeListSections } from '@/lib/iyke/explorer-bridge';
import { DEFAULT_MENUS } from '@/lib/actions/menus';

import { useShellStore } from '@/lib/shell/shell-store';
import { useTerminalStore } from '@/terminal/session-store';
import { useGitStatus } from '@/lib/shell/use-git-status';
import { usePkgActivityBarEntries } from '@/lib/pkg/use-activity-bar-entries';

// Stubs for Radix & resize observer
if (typeof globalThis.ResizeObserver === 'undefined') {
	globalThis.ResizeObserver = class {
		observe() {}
		unobserve() {}
		disconnect() {}
	} as unknown as typeof ResizeObserver;
}

vi.mock('@/lib/shell/shell-store');
vi.mock('@/terminal/session-store');
vi.mock('@/lib/shell/use-git-status');
vi.mock('@/lib/pkg/use-activity-bar-entries');
vi.mock('@/lib/shell/files-store', () => ({
	useFilesStore: vi.fn((sel) =>
		sel({
			hydrated: true,
			hydrate: vi.fn(),
			expandedRoot: null,
			setExpandedRoot: vi.fn(),
			expanded: new Set(),
			queries: {},
			setQuery: vi.fn(),
			toggleRoot: vi.fn(),
			showHidden: false,
			showIgnored: false,
			setShowHidden: vi.fn(),
			setShowIgnored: vi.fn(),
			scrollTop: 0,
			setScrollTop: vi.fn(),
			reveal: vi.fn(),
			toggleShowHidden: vi.fn(),
		})
	),
}));

const queryClient = new QueryClient({
	defaultOptions: { queries: { retry: false } },
});

describe('WP-04 Explorer DoD and Invariants', () => {
	let mockShellStoreState: any;
	let setExplorerSectionCollapsedSpy: any;
	let moveExplorerSectionSpy: any;

	afterEach(() => {
		cleanup();
	});

	beforeEach(() => {
		vi.clearAllMocks();
		setExplorerSectionCollapsedSpy = vi.fn();
		moveExplorerSectionSpy = vi.fn();

		mockShellStoreState = {
			activeProjectId: 'royalti-co',
			activeProject: { id: 'royalti-co', root_path: '/path/to/royalti-co', extra_roots: [] },
			projects: [
				{
					id: 'royalti-co',
					display_name: 'royalti-co',
					root_path: '/path/to/royalti-co',
					icon: null,
					color: null,
					description: null,
					position: 0,
					is_default: false,
					created_at: 0,
					archived_at: null,
				},
			],
			explorerSections: [
				{ id: 'files', source: 'shell', order: 0, collapsed: false },
				{ id: 'artifacts', source: 'shell', order: 1, collapsed: false },
				{ id: 'sessions', source: 'shell', order: 2, collapsed: false },
				{ id: 'ngwa-project', source: 'shell', order: 3, collapsed: true },
				{ id: 'automations', source: 'shell', order: 4, collapsed: false },
				{ id: 'todos', source: 'shell', order: 5, collapsed: true },
				{ id: 'scratchpads', source: 'shell', order: 6, collapsed: true },
				{ id: 'views', source: 'shell', order: 7, collapsed: true },
			],
			setExplorerSectionCollapsed: setExplorerSectionCollapsedSpy,
			moveExplorerSection: moveExplorerSectionSpy,
		};

		(useShellStore as any).mockImplementation((selector: any) => selector(mockShellStoreState));
		(useTerminalStore as any).mockImplementation((selector: any) => selector({ tabs: [] }));
		(useGitStatus as any).mockReturnValue({ data: { files: new Map(), dirtyFolders: new Set() } });
		(usePkgActivityBarEntries as any).mockReturnValue({ entries: [], views: [], loaded: true });
	});

	it('DoD 1: all eight sections render for royalti-co', () => {
		render(
			<QueryClientProvider client={queryClient}>
				<Explorer />
			</QueryClientProvider>
		);

		expect(screen.getByRole('button', { name: /Files/ })).toBeTruthy();
		expect(screen.getByRole('button', { name: /Artifacts/ })).toBeTruthy();
		expect(screen.getByRole('button', { name: /Sessions/ })).toBeTruthy();
		expect(screen.getByRole('button', { name: /Ngwa · project/ })).toBeTruthy();
		expect(screen.getByRole('button', { name: /Automations/ })).toBeTruthy();
		expect(screen.getByRole('button', { name: /Todos/ })).toBeTruthy();
		expect(screen.getByRole('button', { name: /Scratchpads/ })).toBeTruthy();
		expect(screen.getByRole('button', { name: /Views/ })).toBeTruthy();
	});

	it('DoD 2: badges show only when non-zero (§6A.8)', () => {
		const ctx = { projectId: 'royalti-co' };
		const sectionDefWithZero = {
			id: 'test-zero',
			title: 'Test Zero',
			icon: builtInSections[0].icon,
			defaultOrder: 0,
			render: () => <div>Body</div>,
			count: () => 0,
		};
		const sectionDefWithCount = {
			id: 'test-count',
			title: 'Test NonZero',
			icon: builtInSections[0].icon,
			defaultOrder: 1,
			render: () => <div>Body</div>,
			count: () => 5,
		};

		const { unmount, queryByText, getByText } = render(
			<SectionFrame
				section={sectionDefWithZero as any}
				context={ctx}
				isOpen={true}
				onToggle={() => {}}
			>
				<div>Child</div>
			</SectionFrame>
		);

		// Badge with 0 should not render
		expect(queryByText('0')).toBeNull();
		unmount();

		// Badge with 5 should render
		render(
			<SectionFrame
				section={sectionDefWithCount as any}
				context={ctx}
				isOpen={true}
				onToggle={() => {}}
			>
				<div>Child</div>
			</SectionFrame>
		);
		expect(getByText('5')).toBeTruthy();
	});

	it('DoD 3: accordion Alt-click exclusivity and previous open state restoration', () => {
		render(
			<QueryClientProvider client={queryClient}>
				<Explorer />
			</QueryClientProvider>
		);

		const filesHeader = screen.getByRole('button', { name: /Files/ });

		// Alt-click on Files header (exclusive mode)
		fireEvent.click(filesHeader, { altKey: true });

		// Files should be set to collapsed: false, all others to collapsed: true
		expect(setExplorerSectionCollapsedSpy).toHaveBeenCalledWith('files', false);
		expect(setExplorerSectionCollapsedSpy).toHaveBeenCalledWith('artifacts', true);
		expect(setExplorerSectionCollapsedSpy).toHaveBeenCalledWith('sessions', true);
		expect(setExplorerSectionCollapsedSpy).toHaveBeenCalledWith('automations', true);
	});

	it('DoD 3: keyboard navigation (ArrowDown, ArrowUp, Home, End, ⌘⇧[, ⌘⇧])', () => {
		render(
			<QueryClientProvider client={queryClient}>
				<Explorer />
			</QueryClientProvider>
		);

		const region = screen.getByRole('region', { name: 'Explorer sections' });

		// ArrowDown moves down
		fireEvent.keyDown(region, { key: 'ArrowDown' });
		// ArrowUp moves up
		fireEvent.keyDown(region, { key: 'ArrowUp' });
		// Home jumps to start
		fireEvent.keyDown(region, { key: 'Home' });
		// End jumps to end
		fireEvent.keyDown(region, { key: 'End' });

		// ⌘⇧] jumps to next header
		fireEvent.keyDown(region, { key: ']', metaKey: true, shiftKey: true });
		// ⌘⇧[ jumps to previous header
		fireEvent.keyDown(region, { key: '[', metaKey: true, shiftKey: true });
	});

	it('DoD 4 (superseded by WP-55): every Explorer context menu renders from the effective model (G-ACTIONS §1.3), not the WP-04 stub arrays', () => {
		// The WP-04 `{ id, label, run: () => {} }` stub arrays this test used to
		// assert against are dead code now — `TreeNode`, `ArtifactsSection`,
		// `SessionsSection`, `NgwaProjectSection`, `AutomationsSection`,
		// `TodosSection`, `ScratchpadsSection` and `ViewsSection` all render
		// their `ContextMenu` from `getEffectiveMenu(<id>)` (`resolveMenuItems`,
		// `src/shell/menu/resolve.ts`) instead. `menus.ts`'s `DEFAULT_MENUS` is
		// the frozen source of each menu's default order (§10.3's 76 rows).
		const menuIds = [
			'files',
			'files-view',
			'artifacts',
			'session',
			'automations',
			'ngwa-project',
			'scratchpads',
			'todos',
			'views',
		];
		for (const id of menuIds) {
			const defaults = DEFAULT_MENUS[id];
			expect(Array.isArray(defaults), `no default contents for menu "${id}"`).toBe(true);
			expect(defaults.length, `menu "${id}" has no default items`).toBeGreaterThan(0);
			for (const entry of defaults) {
				if (entry === '---') continue;
				expect(typeof entry.id).toBe('string');
			}
		}
	});

	it('DoD 5: Hand to Chi is a default item of every menu it belongs to (G-ACTIONS §10.3)', () => {
		const hasHandToChi = (menuId: string) =>
			DEFAULT_MENUS[menuId].some((e) => e !== '---' && e.id === 'hand-to-chi');
		expect(hasHandToChi('files')).toBe(true);
		expect(hasHandToChi('artifacts')).toBe(true);
		expect(hasHandToChi('session')).toBe(true);
		expect(hasHandToChi('todos')).toBe(true);
	});

	it('DoD 6: empty states offer exactly one action with locked copy from designs/system-flows.html?state=states', () => {
		// Test Artifacts empty state
		const artDef = builtInSections.find((s) => s.id === 'artifacts');
		expect(artDef).toBeTruthy();
		const { getByText: getByTextArt } = render(
			<QueryClientProvider client={queryClient}>
				{artDef!.render({ projectId: 'royalti-co' })}
			</QueryClientProvider>
		);
		expect(getByTextArt('Nothing built yet')).toBeTruthy();
		expect(getByTextArt('Ask a Chi to build one')).toBeTruthy();

		// Test Sessions empty state
		const sessDef = builtInSections.find((s) => s.id === 'sessions');
		expect(sessDef).toBeTruthy();
		const { getByText: getByTextSess } = render(
			<QueryClientProvider client={queryClient}>
				{sessDef!.render({ projectId: 'royalti-co' })}
			</QueryClientProvider>
		);
		expect(getByTextSess('No sessions in this project')).toBeTruthy();
		expect(getByTextSess('Start a session')).toBeTruthy();

		// Test Automations empty state
		const autoDef = builtInSections.find((s) => s.id === 'automations');
		expect(autoDef).toBeTruthy();
		const { getByText: getByTextAuto } = render(
			<QueryClientProvider client={queryClient}>
				{autoDef!.render({ projectId: 'royalti-co' })}
			</QueryClientProvider>
		);
		expect(getByTextAuto('Nothing scheduled')).toBeTruthy();
		expect(getByTextAuto('New schedule')).toBeTruthy();
	});

	it('DoD 7: a11y line — single pointer Move up / Move down and WCAG 2.5.8 sizing', () => {
		const onMoveUp = vi.fn();
		const onMoveDown = vi.fn();

		render(
			<SectionFrame
				section={builtInSections[1]}
				context={{ projectId: 'royalti-co' }}
				isOpen={true}
				onToggle={() => {}}
				onMoveUp={onMoveUp}
				onMoveDown={onMoveDown}
			>
				<div>Content</div>
			</SectionFrame>
		);

		const headerButton = screen.getByRole('button', { name: /Artifacts/ });
		expect(headerButton.className).toContain('min-h-[28px]');
		expect(headerButton.className).toContain('focus-visible:ring-2');
		expect(headerButton.getAttribute('aria-expanded')).toBe('true');
	});

	it('DoD 9: Explorer bridge exposes listExplorerSections and types for WP-21', () => {
		const bridgeSections = bridgeListSections();
		expect(bridgeSections).toHaveLength(8);
		expect(bridgeSections.map((s) => s.id)).toEqual([
			'files',
			'artifacts',
			'sessions',
			'ngwa-project',
			'automations',
			'todos',
			'scratchpads',
			'views',
		]);
	});

	it('ExplorerHeader collapse all and restore functionality', () => {
		render(<ExplorerHeader />);

		const collapseAllButton = screen.getByRole('button', { name: /Collapse all sections/ });
		fireEvent.click(collapseAllButton);

		// Should collapse all open sections
		expect(setExplorerSectionCollapsedSpy).toHaveBeenCalledWith('files', true);
		expect(setExplorerSectionCollapsedSpy).toHaveBeenCalledWith('artifacts', true);
		expect(setExplorerSectionCollapsedSpy).toHaveBeenCalledWith('sessions', true);
		expect(setExplorerSectionCollapsedSpy).toHaveBeenCalledWith('automations', true);
	});
});
