// WP-68 — the `/chi` seat board (D-09 `seats-board.html`): the roster, its
// figures and the not-reported rule, board-local selection vs the dispatch
// target, the rail's own menu and actions, the vacant detail, the empty
// state, the "moved" highlight, keyboard roving, and the entry points.

import { QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { SeatView } from '@/lib/tauri-cmd';

const m = vi.hoisted(() => ({
	seatsList: vi.fn(),
	seatsEngines: vi.fn(async () => []),
	seatsRemove: vi.fn(async () => ({ seat_id: 'x' })),
	seatsClear: vi.fn(async () => ({})),
	seatsGet: vi.fn(),
	seatsRename: vi.fn(async () => ({})),
	seatsResolve: vi.fn(async () => ({})),
	seatsCreate: vi.fn(),
	seatsMove: vi.fn(),
	spawnWindow: vi.fn(async () => 'w2'),
}));

vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	listen: vi.fn(() => Promise.resolve(() => {})),
}));
vi.mock('@/lib/iyke/client', () => ({
	iykeFetch: vi.fn(async () => ({ ok: false, json: async () => ({}) })),
}));
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	seatsList: m.seatsList,
	seatsEngines: m.seatsEngines,
	seatsRemove: m.seatsRemove,
	seatsClear: m.seatsClear,
	seatsGet: m.seatsGet,
	seatsRename: m.seatsRename,
	seatsResolve: m.seatsResolve,
	seatsCreate: m.seatsCreate,
	seatsMove: m.seatsMove,
	spawnWindow: m.spawnWindow,
	listWindows: vi.fn(async () => []),
	chiList: vi.fn(async () => []),
	detectAgents: vi.fn(async () => []),
	ptyTerminalList: vi.fn(async () => []),
	settingsGet: vi.fn(async () => null),
	settingsSet: vi.fn(async () => {}),
}));

import { findLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import { queryClient } from '@/lib/query-client';
import { useShellStore } from '@/lib/shell/shell-store';
import { useDetachedSurfaces } from '@/lib/window/detached-surfaces';
import {
	__resetCompanionTimersForTests,
	useCompanionStore,
} from '@/shell/companion/companion-store';
import { __resetSeatUndoForTests, openSeatBoard, useSeatUi } from '@/shell/companion/seat-actions';
import { UNREPORTED } from '@/shell/companion/seat-model';
import { useSeatNotice } from '@/shell/companion/seat-notice';
import {
	__resetFiguresFeedForTests,
	__resetSessionNumbersForTests,
	sessionNumber,
	useSessionFiguresStore,
} from '@/shell/companion/seat-sessions';
import { useTerminalStore } from '@/terminal/session-store';
import {
	__resetBoardUiForTests,
	boardIsShowing,
	focusBesideBoard,
	openBoard,
	useBoardUi,
} from './board-store';
import { MOVED_MS, opensInPane, SeatBoard } from './seat-board';
import { SessionsSeatsLink } from '@/shell/explorer/sections/sessions';

const PROJECT = 'royalti-co';

function seat(over: Partial<SeatView>): SeatView {
	const name = over.name ?? 'lead';
	return {
		id: `seat-${name}`,
		project_id: PROJECT,
		name,
		engine_id: 'claude-code',
		session: null,
		created_at: 0,
		last_active_at: Date.now() - 2 * 3600_000,
		hold: null,
		address: `seat:${PROJECT}/${name}`,
		agent_id: `seat-${name}`,
		status: 'vacant',
		agent: null,
		resume: { resumable: false, reason: 'no_session' },
		engine_resume: 'durable',
		mount: null,
		queued: null,
		pad: { count: 0, latest: null },
		inbox_count: 0,
		...over,
	};
}

/** D-09 sample content, as the host would report it. */
function roster(): SeatView[] {
	return [
		seat({
			name: 'lead',
			status: 'live',
			agent: 'live',
			session: { kind: 'terminal', terminal_id: 'term-3', external_id: 'c3', cwd: '/w' },
			resume: { resumable: true },
			pad: { count: 3, latest: { name: 'WP-64 brief drafted', updated_at: 0 } },
		}),
		seat({
			name: 'review',
			engine_id: 'codex',
			status: 'idle',
			agent: 'unreported',
			session: { kind: 'terminal', terminal_id: 'term-1', external_id: null, cwd: '/w' },
			resume: { resumable: true },
			pad: { count: 1, latest: { name: 'waiting on lead’s diff', updated_at: 0 } },
		}),
		seat({
			name: 'nightly',
			status: 'run',
			session: { kind: 'run', run_id: 'run-np', external_id: 'x', cwd: '/w' },
			resume: { resumable: true },
			pad: { count: 2, latest: null },
			inbox_count: 1,
		}),
		seat({
			name: 'docs',
			status: 'vacant',
			session: { kind: 'terminal', terminal_id: 'term-2', external_id: 'c2', cwd: '/w' },
			resume: { resumable: true },
			pad: { count: 5, latest: { name: 'STATUS.md pass half done', updated_at: 0 } },
		}),
	];
}

function tab(id: string, engine: 'claude' | 'codex' | null, createdAt: number) {
	return {
		id,
		title: id,
		spec: { cwd: '/w', cmd: ['bash'], ...(engine ? { wrap: { engine } } : {}) },
		ptyId: `pty-${id}`,
		status: 'running' as const,
		exitCode: null,
		createdAt,
		owner: { kind: 'sidepane' as const },
	};
}

function wrap(ui: ReactNode) {
	return render(<QueryClientProvider client={queryClient}>{ui}</QueryClientProvider>);
}

function board(): HTMLElement {
	return document.querySelector('.chi-board') as HTMLElement;
}

function row(key: string): HTMLElement {
	return document.querySelector(`[data-row-key="${key}"]`) as HTMLElement;
}

async function mountBoard(seats: SeatView[] = roster()) {
	m.seatsList.mockResolvedValue(seats);
	const r = wrap(<SeatBoard />);
	await waitFor(() => expect(board().dataset.state).not.toBe('board-loading'));
	return r;
}

beforeEach(() => {
	queryClient.clear();
	__resetCompanionTimersForTests();
	__resetSeatUndoForTests();
	__resetSessionNumbersForTests();
	__resetFiguresFeedForTests();
	__resetBoardUiForTests();
	m.seatsList.mockReset();
	useSeatNotice.setState({ notice: null });
	useDetachedSurfaces.setState({ surfaceToWindow: {} });
	// Project first: switching it resets the Companion target and the rail
	// selection (companion-store's project subscription), which the first
	// test in the file would otherwise see.
	useShellStore.setState({
		activeProjectId: PROJECT,
		activeProject: { id: PROJECT, root_path: '/w', extra_roots: [] },
		projects: [],
		defaultEngineId: 'claude-code',
	});
	useShellStore.setState({ companion: { activeTarget: { kind: 'seat', seat_id: 'seat-lead' } } });
	useCompanionStore.setState({
		state: 'expanded',
		tabs: [],
		activeIdx: 0,
		width: 372,
		panelScopeSessionId: 'term-3',
		railSelection: { kind: 'seat', seat_id: 'seat-lead' },
		draft: '',
		focusPending: false,
		pickerPending: false,
		permissions: [],
		quietSince: null,
	});
	useTerminalStore.setState({
		tabs: [
			tab('term-1', 'codex', 1),
			tab('term-2', 'claude', 2),
			tab('term-3', 'claude', 3),
			tab('term-4', 'claude', 4),
		],
	} as never);
	usePaneStore.setState({
		root: {
			type: 'split',
			direction: 'horizontal',
			sizes: [50, 50],
			children: [
				{
					type: 'leaf',
					id: 'L1',
					tabs: [{ kind: 'terminal', sessionId: 'term-3' }],
					activeTabIdx: 0,
				},
				{ type: 'leaf', id: 'L2', tabs: [{ kind: 'route', path: '/chi' }], activeTabIdx: 0 },
			],
		},
		focusedId: 'L2',
	});
});

afterEach(() => {
	cleanup();
	__resetSeatUndoForTests();
	vi.useRealTimers();
	useTerminalStore.setState({ tabs: [] } as never);
});

describe('roster (D-09 default state)', () => {
	it('lists every seat, then the Unseated group, with the rail’s selection selected', async () => {
		await mountBoard();
		expect(board().dataset.state).toBe('board-roster');
		const grid = screen.getByRole('grid', { name: `Seats in ${PROJECT}` });
		const rows = Array.from(grid.querySelectorAll<HTMLElement>('[data-row-key]')).map(
			(r) => r.dataset.rowKey
		);
		expect(rows).toEqual([
			'seat:seat-lead',
			'seat:seat-review',
			'seat:seat-nightly',
			'seat:seat-docs',
			'session:term-4',
		]);
		expect(within(grid).getByText('Unseated sessions')).toBeTruthy();
		expect(screen.getByText(`${PROJECT} · 4 seats · 1 vacant · 1 unseated session`)).toBeTruthy();
		// The rail selected @lead; the board opens on it, detail column included.
		expect(row('seat:seat-lead').getAttribute('aria-selected')).toBe('true');
		expect(document.querySelector('[data-board-detail="seat:seat-lead"]')).not.toBeNull();
		expect(screen.getByRole('heading', { name: '@lead' })).toBeTruthy();
		// One tab stop in the grid.
		const stops = Array.from(grid.querySelectorAll('[role="row"][tabindex="0"]'));
		expect(stops).toHaveLength(1);
	});

	it('the row carries session, state, scratchpad (full seat: address) and mount', async () => {
		await mountBoard();
		const lead = row('seat:seat-lead');
		expect(lead.textContent).toContain(`session ${sessionNumber('term-3')}`);
		expect(lead.querySelector('.state.s-live')?.textContent).toBe('live');
		expect(lead.textContent).toContain('seat:royalti-co/lead');
		expect(lead.textContent).toContain('“WP-64 brief drafted”');
		expect(lead.textContent).toContain('pane 1');
		expect(row('seat:seat-nightly').textContent).toContain('headless');
		expect(row('seat:seat-nightly').querySelector('.pp.inbox')?.textContent).toBe('1');
		expect(row('seat:seat-docs').querySelector('.state.s-vacant')).not.toBeNull();
		expect(row('seat:seat-docs').textContent).toContain(`session ${sessionNumber('term-2')} ended`);
	});

	it('figures an engine didn’t report read "—" with the not-reported tooltip; reported ones show', async () => {
		useSessionFiguresStore.setState({
			snaps: {
				'term-3': { cost: { total_cost_usd: 1.42 }, context_window: { total_input_tokens: 38120 } },
			},
		});
		await mountBoard();
		const lead = row('seat:seat-lead');
		expect(lead.querySelector('.c-ctx')?.textContent).toBe('38k');
		expect(lead.querySelector('.c-cost')?.textContent).toBe('$1.42');
		for (const key of ['seat:seat-review', 'seat:seat-nightly']) {
			const ctx = row(key).querySelector('.c-ctx .num') as HTMLElement;
			const cost = row(key).querySelector('.c-cost .num') as HTMLElement;
			expect(ctx.textContent, key).toBe('—');
			expect(ctx.getAttribute('title'), key).toBe(UNREPORTED);
			expect(cost.textContent, key).toBe('—');
			expect(cost.getAttribute('title'), key).toBe(UNREPORTED);
		}
		// The detail's exact context, as reported.
		expect(document.querySelector('[data-board-detail]')?.textContent).toContain('38,120 tokens');
	});

	it('a pending permission rides the row and the detail offers Review', async () => {
		useCompanionStore
			.getState()
			.receivePermission({ id: 'p1', kind: 'permission', toolName: 'Read', sessionId: 'term-3' });
		await mountBoard();
		expect(row('seat:seat-lead').querySelector('.c-pend .pp')?.getAttribute('title')).toBe(
			'1 permission pending'
		);
		fireEvent.click(document.querySelector('[data-board-review]') as HTMLElement);
		expect(useCompanionStore.getState().state).toBe('expanded');
		expect(useShellStore.getState().companion.activeTarget).toEqual({
			kind: 'seat',
			seat_id: 'seat-lead',
		});
	});

	it('a hold reads "held by X since T" (§5.2)', async () => {
		const seats = roster();
		seats[1] = {
			...seats[1],
			hold: {
				client: 'orchestrator',
				since: Date.now() - 60_000,
				expires_at: Date.now() + 600_000,
			},
		};
		await mountBoard(seats);
		expect(
			row('seat:seat-review').querySelector('.c-pend [aria-label^="held by orchestrator since"]')
		).not.toBeNull();
		fireEvent.click(row('seat:seat-review'));
		expect(document.querySelector('[data-board-detail]')?.textContent).toContain(
			'held by orchestrator since'
		);
	});
});

describe('selection is board-local; the target moves only on purpose', () => {
	it('browsing rows never retargets dispatch; Make dispatch target does', async () => {
		await mountBoard();
		fireEvent.click(row('seat:seat-review'));
		expect(row('seat:seat-review').getAttribute('aria-selected')).toBe('true');
		expect(useBoardUi.getState().selection).toEqual({ kind: 'seat', id: 'seat-review' });
		expect(useShellStore.getState().companion.activeTarget).toEqual({
			kind: 'seat',
			seat_id: 'seat-lead',
		});
		expect(useCompanionStore.getState().railSelection).toEqual({
			kind: 'seat',
			seat_id: 'seat-lead',
		});

		fireEvent.click(document.querySelector('[data-board-make-target]') as HTMLElement);
		expect(useShellStore.getState().companion.activeTarget).toEqual({
			kind: 'seat',
			seat_id: 'seat-review',
		});
		expect(useCompanionStore.getState().panelScopeSessionId).toBe('term-1');
		// It is the target now: the button goes, the chip shows.
		await waitFor(() => expect(document.querySelector('[data-board-make-target]')).toBeNull());
		expect(screen.getByText('dispatch target')).toBeTruthy();
	});

	it('↑ ↓ Home End rove the grid; Enter opens the seat’s terminal beside the board', async () => {
		await mountBoard();
		const lead = row('seat:seat-lead');
		lead.focus();
		fireEvent.keyDown(lead, { key: 'ArrowDown' });
		expect(useBoardUi.getState().selection).toEqual({ kind: 'seat', id: 'seat-review' });
		fireEvent.keyDown(row('seat:seat-review'), { key: 'End' });
		expect(useBoardUi.getState().selection).toEqual({ kind: 'session', id: 'term-4' });
		fireEvent.keyDown(row('session:term-4'), { key: 'Home' });
		expect(useBoardUi.getState().selection).toEqual({ kind: 'seat', id: 'seat-lead' });
		// Enter: the board's pane (L2) stays the board; the terminal lands in L1.
		fireEvent.keyDown(row('session:term-4'), { key: 'Enter' });
		const { root, focusedId } = usePaneStore.getState();
		expect(focusedId).toBe('L1');
		expect(
			findLeaf(root, 'L1')?.tabs.some((t) => t.kind === 'terminal' && t.sessionId === 'term-4')
		).toBe(true);
		expect(findLeaf(root, 'L2')?.tabs).toEqual([{ kind: 'route', path: '/chi' }]);
		// Still no retarget.
		expect(useShellStore.getState().companion.activeTarget).toEqual({
			kind: 'seat',
			seat_id: 'seat-lead',
		});
	});
});

describe('the rail’s menu and actions, from the board', () => {
	it('right-click and ⋯ open the rail’s own seat menu; Esc closes it', async () => {
		await mountBoard();
		fireEvent.contextMenu(row('seat:seat-review'));
		const menu = screen.getByRole('menu', { name: 'Seat actions for @review' });
		const labels = within(menu)
			.getAllByRole('menuitem')
			.map((b) => b.querySelector('span')?.textContent);
		expect(labels).toEqual([
			'Open in pane',
			'Make dispatch target',
			'Open scratchpad',
			'Pop out',
			'All seats',
			'Rename…',
			'Copy address',
			'Copy as iyke',
			'End session',
			'Remove seat…',
		]);
		// Right-click selected the row on the board, not the target.
		expect(useBoardUi.getState().selection).toEqual({ kind: 'seat', id: 'seat-review' });
		expect(useShellStore.getState().companion.activeTarget).toEqual({
			kind: 'seat',
			seat_id: 'seat-lead',
		});
		fireEvent.keyDown(menu, { key: 'Escape' });
		expect(screen.queryByRole('menu')).toBeNull();

		fireEvent.click(
			within(row('seat:seat-lead')).getByRole('button', { name: 'Seat actions for @lead' })
		);
		expect(screen.getByRole('menu', { name: 'Seat actions for @lead' })).toBeTruthy();
	});

	it('Open scratchpad from the menu opens seat:<project>/<name> beside the board', async () => {
		await mountBoard();
		fireEvent.contextMenu(row('seat:seat-lead'));
		fireEvent.click(screen.getByRole('menuitem', { name: /Open scratchpad/ }));
		const { root, focusedId } = usePaneStore.getState();
		expect(focusedId).toBe('L1');
		expect(
			findLeaf(root, 'L1')?.tabs.some(
				(t) => t.kind === 'scratchpad' && t.scope === 'seat:royalti-co/lead'
			)
		).toBe(true);
		expect(findLeaf(root, 'L2')?.tabs).toEqual([{ kind: 'route', path: '/chi' }]);
	});

	it('Rename… goes to the rail’s inline rename (the Companion comes forward)', async () => {
		useCompanionStore.setState({ state: 'collapsed' });
		await mountBoard();
		fireEvent.contextMenu(row('seat:seat-review'));
		fireEvent.click(screen.getByRole('menuitem', { name: /Rename…/ }));
		expect(useCompanionStore.getState().state).toBe('expanded');
		expect(useSeatUi.getState().renaming).toBe('seat-review');
		// Not a pane-opening item: pane focus stays on the board's pane.
		expect(usePaneStore.getState().focusedId).toBe('L2');
	});

	it('only the items that open something in a pane move pane focus off the board', async () => {
		expect(opensInPane('Open in pane')).toBe(true);
		expect(opensInPane('Open in pane — back to main window')).toBe(true);
		expect(opensInPane('Open scratchpad')).toBe(true);
		expect(opensInPane('Move to pane')).toBe(true);
		for (const label of [
			'All seats',
			'Make dispatch target',
			'Copy address',
			'Copy as iyke',
			'Rename…',
			'Pop out',
			'End session',
			'Remove seat…',
		]) {
			expect(opensInPane(label)).toBe(false);
		}
		await mountBoard();
		fireEvent.contextMenu(row('seat:seat-review'));
		fireEvent.click(screen.getByRole('menuitem', { name: /Make dispatch target/ }));
		expect(useShellStore.getState().companion.activeTarget).toEqual({
			kind: 'seat',
			seat_id: 'seat-review',
		});
		expect(usePaneStore.getState().focusedId).toBe('L2');
	});

	it('Remove seat… asks the rail’s confirm; with the Companion hidden the board hosts it', async () => {
		useCompanionStore.setState({ state: 'hidden' });
		await mountBoard();
		fireEvent.contextMenu(row('seat:seat-docs'));
		fireEvent.click(screen.getByRole('menuitem', { name: /Remove seat…/ }));
		expect(useSeatUi.getState().confirmRemove).toBe('seat-docs');
		expect(usePaneStore.getState().focusedId).toBe('L2');
		expect(await screen.findByRole('button', { name: 'Keep it' })).toBeTruthy();
	});

	it('New seat and Seat this session… open the Companion’s form', async () => {
		useCompanionStore.setState({ state: 'collapsed' });
		await mountBoard();
		fireEvent.click(document.querySelector('[data-board-new-seat]') as HTMLElement);
		expect(useSeatUi.getState().form).toEqual({});
		expect(useCompanionStore.getState().state).toBe('expanded');
		await waitFor(() => expect(board().dataset.state).toBe('board-create'));
		act(() => useSeatUi.setState({ form: null }));
		fireEvent.click(
			within(row('session:term-4')).getByRole('button', { name: /Seat this session…/ })
		);
		expect(useSeatUi.getState().form).toEqual({ seatSession: 'term-4' });
	});

	it('the iyke line names the selected row, and Copy copies it', async () => {
		await mountBoard();
		expect(document.querySelector('[data-iyke-line]')?.textContent).toBe(
			'terminal-send --seat lead "…"'
		);
		fireEvent.click(row('seat:seat-docs'));
		expect(document.querySelector('[data-iyke-line]')?.textContent).toBe(
			'seat resume docs --prompt "…"'
		);
		// jsdom has no clipboard; give it a working one. Without it the copy now
		// (correctly) fails and shows an error, not a false "Copied".
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
		fireEvent.click(document.querySelector('[data-board-iyke-copy]') as HTMLElement);
		// The notice waits for the clipboard write to succeed (no false "Copied").
		await waitFor(() =>
			expect(useSeatNotice.getState().notice?.message).toBe(
				'Copied iyke seat resume docs --prompt "…"'
			)
		);
		expect(writeText).toHaveBeenCalledWith('iyke seat resume docs --prompt "…"');
		Reflect.deleteProperty(navigator, 'clipboard');
	});
});

describe('vacant, empty, popout', () => {
	it('a vacant seat selected: board-vacant, with the rail’s own Resume / Fill / Clear', async () => {
		await mountBoard();
		fireEvent.click(row('seat:seat-docs'));
		expect(board().dataset.state).toBe('board-vacant');
		const detail = document.querySelector('[data-board-detail="seat:seat-docs"]') as HTMLElement;
		expect(detail.querySelector('[data-state="seats-vacant"]')).not.toBeNull();
		expect(
			within(detail).getByRole('button', { name: `Resume session ${sessionNumber('term-2')}` })
		).toBeTruthy();
		expect(within(detail).getByRole('button', { name: 'Fill with a new session' })).toBeTruthy();
		expect(within(detail).getByRole('button', { name: 'Clear seat' })).toBeTruthy();
		expect(detail.querySelector('[data-promise]')?.textContent).toContain('@docs');
	});

	it('empty: one sentence, two actions, and the sessions unseated below', async () => {
		useCompanionStore.setState({ railSelection: null });
		useShellStore.setState({ companion: { activeTarget: { kind: 'new', engine_id: null } } });
		await mountBoard([]);
		expect(board().dataset.state).toBe('board-empty');
		expect(
			screen.getByText('A seat keeps an agent’s name when its pane moves or its session ends.')
		).toBeTruthy();
		expect(document.querySelector('[data-board-empty-new]')).not.toBeNull();
		expect((document.querySelector('[data-board-empty-seat]') as HTMLButtonElement).disabled).toBe(
			false
		);
		const unseated = Array.from(
			document.querySelectorAll<HTMLElement>('[data-row-key^="session:"]')
		).map((r) => r.dataset.session);
		expect(unseated.length).toBeGreaterThan(0);
		// No "New seat" in the head until a seat exists (D-09).
		expect(document.querySelector('[data-board-new-seat]')).toBeNull();
	});

	it('Pop out: the Window 2 chip carries the "moved" highlight (G-93, G-96), then settles', async () => {
		await mountBoard();
		vi.useFakeTimers();
		act(() =>
			useDetachedSurfaces.setState({ surfaceToWindow: { 'terminal:pty-term-3': 'detached-1' } })
		);
		const chip = row('seat:seat-lead').querySelector('.w2chip') as HTMLElement;
		expect(chip).not.toBeNull();
		expect(chip.textContent).toContain('Window 2');
		expect(chip.dataset.moved).toBe('true');
		expect(board().dataset.state).toBe('board-popout');
		act(() => {
			vi.advanceTimersByTime(MOVED_MS + 10);
		});
		expect(
			(row('seat:seat-lead').querySelector('.w2chip') as HTMLElement).dataset.moved
		).toBeUndefined();
		expect(board().dataset.state).toBe('board-roster');
	});

	it('a row already in Window 2 when the board opens is not "moved"', async () => {
		useDetachedSurfaces.setState({ surfaceToWindow: { 'terminal:pty-term-1': 'detached-1' } });
		await mountBoard();
		const chip = row('seat:seat-review').querySelector('.w2chip') as HTMLElement;
		expect(chip.dataset.moved).toBeUndefined();
		expect(board().dataset.state).toBe('board-roster');
	});
});

describe('entry points', () => {
	it('openBoard() opens /chi in the focused pane once — a second open reuses the tab', () => {
		usePaneStore.setState({
			root: {
				type: 'leaf',
				id: 'L1',
				tabs: [{ kind: 'terminal', sessionId: 'term-3' }],
				activeTabIdx: 0,
			},
			focusedId: 'L1',
		});
		const before = useBoardUi.getState().focusRequest;
		openBoard();
		openBoard();
		const leaf = findLeaf(usePaneStore.getState().root, 'L1');
		expect(leaf?.tabs.filter((t) => t.kind === 'route' && t.path === '/chi')).toHaveLength(1);
		expect(boardIsShowing(usePaneStore.getState().root)).toBe(true);
		expect(useBoardUi.getState().focusRequest).toBe(before + 2);
	});

	it('an entry point lands keyboard focus on the selected row', async () => {
		await mountBoard();
		act(() => openBoard());
		await waitFor(() => expect(document.activeElement).toBe(row('seat:seat-lead')));
	});

	it('the rail’s ⊞ / All seats (openSeatBoard) asks for focus too — a mounted board takes it', async () => {
		await mountBoard();
		(document.activeElement as HTMLElement | null)?.blur();
		act(() => openSeatBoard());
		await waitFor(() => expect(document.activeElement).toBe(row('seat:seat-lead')));
		expect(useBoardUi.getState().focusPending).toBe(false);
	});

	it('a request made while the board has no rows is consumed, not left to steal focus later', async () => {
		useTerminalStore.setState({ tabs: [] } as never);
		act(() => openSeatBoard());
		const r = await mountBoard([]);
		await waitFor(() => expect(useBoardUi.getState().focusPending).toBe(false));
		r.unmount();
		queryClient.clear();
		useTerminalStore.setState({ tabs: [tab('term-3', 'claude', 3)] } as never);
		await mountBoard();
		expect(row('seat:seat-lead')).toBeTruthy();
		// No new request: the remount leaves focus where it was.
		expect(document.activeElement).toBe(document.body);
	});

	it('the Explorer Sessions header’s "Seats" link opens the board, and reads as current while it shows', () => {
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
		// WP-71a: the link lives in the Sessions header row (the registry's
		// `headerActions`); `section-frame.test.tsx` covers its placement.
		wrap(<SessionsSeatsLink projectId={PROJECT} />);
		const link = document.querySelector('[data-explorer-seats-link]') as HTMLButtonElement;
		expect(link.textContent).toBe('Seats');
		expect(link.getAttribute('aria-current')).toBeNull();
		fireEvent.click(link);
		const leaf = findLeaf(usePaneStore.getState().root, 'L1');
		expect(leaf?.tabs[leaf.activeTabIdx]).toEqual({ kind: 'route', path: '/chi' });
		expect(link.getAttribute('aria-current')).toBe('page');
		expect(useBoardUi.getState().focusPending).toBe(true);
	});

	it('focusBesideBoard points "the focused pane" away from the board, and never moves otherwise', () => {
		focusBesideBoard('L2');
		expect(usePaneStore.getState().focusedId).toBe('L1');
		// Already beside the board: nothing moves.
		focusBesideBoard('L2');
		expect(usePaneStore.getState().focusedId).toBe('L1');
		// The board alone in the window: nothing to move to.
		usePaneStore.setState({
			root: { type: 'leaf', id: 'L2', tabs: [{ kind: 'route', path: '/chi' }], activeTabIdx: 0 },
			focusedId: 'L2',
		});
		focusBesideBoard('L2');
		expect(usePaneStore.getState().focusedId).toBe('L2');
	});
});
