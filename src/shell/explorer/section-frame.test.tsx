// WP-71a — the Explorer section header's actions slot (`headerActions`,
// `section-registry.ts`) and the Sessions "Seats" link in it (D-09 LISTING,
// `seats-board.html` `.seclink`). Written under DEC-50: not run here.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useShellStore } from '@/lib/shell/shell-store';
import { __resetBoardUiForTests } from '@/shell/chi-board/board-ui';
import { useTerminalStore } from '@/terminal/session-store';
import { SectionFrame } from './section-frame';
import { builtInSections, type ExplorerSectionDefinition } from './section-registry';

if (typeof globalThis.ResizeObserver === 'undefined') {
	globalThis.ResizeObserver = class {
		observe() {}
		unobserve() {}
		disconnect() {}
	} as unknown as typeof ResizeObserver;
}

const CTX = { projectId: 'royalti-co' };
const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });

function wrap(ui: ReactNode) {
	return render(<QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>);
}

function header(name: RegExp): HTMLButtonElement {
	return screen.getByRole('button', { name }) as HTMLButtonElement;
}

const sessionsDef = builtInSections.find((s) => s.id === 'sessions') as ExplorerSectionDefinition;

function renderSessions(isOpen: boolean, onToggle = vi.fn()) {
	wrap(
		<SectionFrame section={sessionsDef} context={CTX} isOpen={isOpen} onToggle={onToggle}>
			{sessionsDef.render(CTX)}
		</SectionFrame>
	);
	return onToggle;
}

function seatsLink(): HTMLButtonElement {
	return document.querySelector('[data-explorer-seats-link]') as HTMLButtonElement;
}

// Round 52: every shared store a test touches is reset here, so no test
// inherits another's panes, terminals or board focus request.
beforeEach(() => {
	// The active project first: Sessions (Start a session, the Seats link)
	// reads it, and its switch resets Companion state.
	useShellStore.setState({
		activeProjectId: 'royalti-co',
		activeProject: { id: 'royalti-co', root_path: '/w', extra_roots: [] },
		projects: [],
	});
	queryClient.clear();
	__resetBoardUiForTests();
	useTerminalStore.setState({ tabs: [] } as never);
	usePaneStore.setState({
		root: {
			type: 'leaf',
			id: 'L1',
			tabs: [{ kind: 'route', path: '/project/dashboard' }],
			activeTabIdx: 0,
		},
		focusedId: 'L1',
	});
});

afterEach(() => {
	cleanup();
});

describe('SectionFrame headerActions slot', () => {
	const plain: ExplorerSectionDefinition = {
		id: 'plain',
		title: 'Plain',
		icon: builtInSections[0].icon,
		defaultOrder: 0,
		render: () => <div>Body</div>,
	};
	const withAction: ExplorerSectionDefinition = {
		...plain,
		id: 'acted',
		title: 'Acted',
		headerActions: () => (
			<button type="button" data-testid="hdr-act">
				Go
			</button>
		),
	};

	it('renders nothing extra, and keeps the plain header padding, when a section declares none', () => {
		wrap(
			<SectionFrame section={plain} context={CTX} isOpen onToggle={() => {}}>
				<div>Body</div>
			</SectionFrame>
		);
		expect(document.querySelector('[data-explorer-header-actions]')).toBeNull();
		expect(header(/Plain/).className).toContain('pr-2');
	});

	it('renders the actions in the header row, beside (not inside) the header button', () => {
		wrap(
			<SectionFrame section={withAction} context={CTX} isOpen onToggle={() => {}}>
				<div>Body</div>
			</SectionFrame>
		);
		const act = screen.getByTestId('hdr-act');
		const btn = header(/Acted/);
		expect(btn.contains(act)).toBe(false);
		// Same row: both sit in the header wrapper, ahead of the body.
		expect(act.closest('[data-explorer-header-actions="acted"]')?.parentElement).toBe(
			btn.parentElement
		);
		// D-09 `.sechead { padding-right: 76px }` clears room for the link.
		expect(btn.className).toContain('pr-[76px]');
	});

	it('shows the actions while the section is collapsed too', () => {
		wrap(
			<SectionFrame section={withAction} context={CTX} isOpen={false} onToggle={() => {}}>
				<div>Body</div>
			</SectionFrame>
		);
		expect(screen.getByTestId('hdr-act')).toBeTruthy();
		expect(screen.queryByText('Body')).toBeNull();
	});

	it('a click on an action never toggles the section', () => {
		const onToggle = vi.fn();
		wrap(
			<SectionFrame section={withAction} context={CTX} isOpen onToggle={onToggle}>
				<div>Body</div>
			</SectionFrame>
		);
		fireEvent.click(screen.getByTestId('hdr-act'));
		expect(onToggle).not.toHaveBeenCalled();
		fireEvent.click(header(/Acted/));
		expect(onToggle).toHaveBeenCalledTimes(1);
	});
});

describe('the Sessions header "Seats" link (D-09 LISTING)', () => {
	it('sits in the Sessions header row, not in the section body', () => {
		renderSessions(true);
		const link = seatsLink();
		expect(link).toBeTruthy();
		expect(link.textContent).toBe('Seats');
		expect(link.closest('[data-explorer-header-actions="sessions"]')).toBeTruthy();
		expect(header(/Sessions/).contains(link)).toBe(false);
		// The body (empty state here) no longer carries a copy.
		expect(document.querySelectorAll('[data-explorer-seats-link]')).toHaveLength(1);
		expect(screen.getByText('No sessions in this project')).toBeTruthy();
	});

	it('stays in the header while the section is collapsed', () => {
		renderSessions(false);
		expect(seatsLink()).toBeTruthy();
		expect(screen.queryByText('No sessions in this project')).toBeNull();
	});

	it('opens the board without collapsing the section, and reads as current while it shows', () => {
		const onToggle = renderSessions(true);
		const link = seatsLink();
		expect(link.getAttribute('aria-current')).toBeNull();
		fireEvent.click(link);
		expect(onToggle).not.toHaveBeenCalled();
		const leaf = findLeaf(usePaneStore.getState().root, 'L1');
		expect(leaf?.tabs[leaf.activeTabIdx]).toEqual({ kind: 'route', path: '/chi' });
		expect(seatsLink().getAttribute('aria-current')).toBe('page');
	});

	it('is current when a pane already shows the board', () => {
		usePaneStore.setState({
			root: { type: 'leaf', id: 'L1', tabs: [{ kind: 'route', path: '/chi' }], activeTabIdx: 0 },
			focusedId: 'L1',
		});
		renderSessions(true);
		expect(seatsLink().getAttribute('aria-current')).toBe('page');
		expect(seatsLink().className).toContain('border-primary');
	});
});
