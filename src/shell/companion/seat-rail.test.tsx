// WP-67 — the seat rail (D-09 `seats-companion.html`): roster, empty,
// create, vacant, dispatch and rest, the seat menu, Remove/Clear with the
// 8 s client-side Undo, ⌥↑/⌥↓, and the rest strip's surviving signals.

import { QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { SeatView } from '@/lib/tauri-cmd';

const m = vi.hoisted(() => ({
	seatsList: vi.fn(),
	seatsEngines: vi.fn(),
	seatsRemove: vi.fn(async () => ({ seat_id: 'x' })),
	seatsClear: vi.fn(async () => ({})),
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
vi.mock('@/lib/iyke/client', () => ({ iykeFetch: vi.fn(async () => ({ ok: false, json: async () => ({}) })) }));
vi.mock('@/lib/tauri-cmd', async (orig) => ({
	...(await orig<typeof import('@/lib/tauri-cmd')>()),
	seatsList: m.seatsList,
	seatsEngines: m.seatsEngines,
	seatsRemove: m.seatsRemove,
	seatsClear: m.seatsClear,
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
vi.mock('@/shell/panes/pane-views', () => ({
	viewLabel: (v: { kind: string; sessionId?: string; path?: string }) =>
		v.kind === 'terminal' ? `terminal ${v.sessionId}` : (v.path ?? v.kind),
}));

import { usePaneStore } from '@/lib/panes/pane-store';
import { queryClient } from '@/lib/query-client';
import { useShellStore } from '@/lib/shell/shell-store';
import { useTerminalStore } from '@/terminal/session-store';
import { Companion } from './companion';
import { __resetCompanionTimersForTests, useCompanionStore } from './companion-store';
import { __resetSeatUndoForTests, clearSeat, removeSeat, useSeatUi } from './seat-actions';
import { useSeatNotice } from './seat-notice';
import { __resetSessionNumbersForTests, sessionNumber } from './seat-sessions';

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

async function mountRail(seats: SeatView[] = roster()) {
	m.seatsList.mockResolvedValue(seats);
	useCompanionStore.setState({ state: 'expanded' });
	const r = wrap(<Companion />);
	if (seats.length) await screen.findByRole('option', { name: /^@lead/ });
	else await waitFor(() => expect(document.querySelector('[data-state="seats-empty"]')).not.toBeNull());
	return r;
}

beforeEach(() => {
	queryClient.clear();
	__resetCompanionTimersForTests();
	__resetSeatUndoForTests();
	__resetSessionNumbersForTests();
	m.seatsList.mockReset();
	m.seatsEngines.mockReset().mockResolvedValue([
		{ engine_id: 'claude-code', wrap_id: 'claude', engine_resume: 'durable', seatable: true },
		{ engine_id: 'codex', wrap_id: 'codex', engine_resume: 'durable', seatable: true },
		{ engine_id: 'gemini', wrap_id: 'gemini', engine_resume: null, seatable: false, reason: 'not installed — Ngwa → Store' },
	]);
	m.seatsRemove.mockClear();
	m.seatsClear.mockClear();
	m.seatsResolve.mockClear();
	m.spawnWindow.mockClear();
	useSeatNotice.setState({ notice: null });
	useCompanionStore.setState({
		state: 'collapsed',
		tabs: [],
		activeIdx: 0,
		width: 372,
		panelScopeSessionId: null,
		railSelection: null,
		draft: '',
		focusPending: false,
		pickerPending: false,
		permissions: [],
		quietSince: null,
	});
	useShellStore.setState({
		activeProjectId: PROJECT,
		activeProject: { id: PROJECT, root_path: '/w', extra_roots: [] },
		companion: { activeTarget: { kind: 'new', engine_id: null } },
		defaultEngineId: 'claude-code',
		onboarding: { ...useShellStore.getState().onboarding, loreGlossSeen: ['chi'] },
	});
	useTerminalStore.setState({
		tabs: [tab('term-1', 'codex', 1), tab('term-2', 'claude', 2), tab('term-3', 'claude', 3), tab('term-4', 'claude', 4)],
	});
	usePaneStore.setState({
		root: { type: 'leaf', id: 'L1', tabs: [{ kind: 'terminal', sessionId: 'term-3' }], activeTabIdx: 0 },
		focusedId: 'L1',
	});
});

afterEach(() => {
	cleanup();
	__resetSeatUndoForTests();
	vi.useRealTimers();
	useTerminalStore.setState({ tabs: [] });
});

describe('roster (D-09 default state)', () => {
	it('lists the seats in order, then the Unseated group', async () => {
		await mountRail();
		const rail = document.querySelector('[data-state="seats-roster"]') as HTMLElement;
		expect(rail).not.toBeNull();
		const options = within(rail).getAllByRole('option');
		expect(options.map((o) => o.getAttribute('aria-label')?.split(',')[0])).toEqual([
			'@lead',
			'@review',
			'@nightly',
			'@docs',
			`claude · session ${sessionNumber('term-4')}`,
		]);
		expect(within(rail).getByText('Unseated')).toBeTruthy();
		// Signals: the mount readout, the inbox, the vacant ring.
		const lead = within(rail).getByRole('option', { name: /^@lead/ });
		expect(lead.textContent).toContain('pane 1');
		expect(within(rail).getByRole('option', { name: /^@nightly/ }).querySelector('[data-signal="inbox"]')).not.toBeNull();
		expect(within(rail).getByRole('option', { name: /^@docs/ }).querySelector('[data-status="vacant"]')).not.toBeNull();
	});

	it('selecting a seat targets it AND scopes the panels to its session (§9.1)', async () => {
		await mountRail();
		fireEvent.click(screen.getByRole('option', { name: /^@lead/ }));
		expect(useShellStore.getState().companion.activeTarget).toEqual({ kind: 'seat', seat_id: 'seat-lead' });
		expect(useCompanionStore.getState().panelScopeSessionId).toBe('term-3');
		expect(useCompanionStore.getState().railSelection).toEqual({ kind: 'seat', seat_id: 'seat-lead' });
		// The chip speaks in seats; the scratchpad line shows the canonical scope.
		const chip = screen.getByRole('button', { name: /^Dispatch target:/ });
		expect(chip.textContent).toBe(`@lead · claude · session ${sessionNumber('term-3')}`);
		expect(screen.getByRole('option', { name: /^@lead/ }).textContent).toContain('seat:royalti-co/lead');
	});

	it('↑ / ↓ rove the rail and select (one tab stop)', async () => {
		await mountRail();
		const lead = screen.getByRole('option', { name: /^@lead/ });
		fireEvent.click(lead);
		fireEvent.keyDown(lead, { key: 'ArrowDown' });
		expect(useShellStore.getState().companion.activeTarget).toEqual({ kind: 'seat', seat_id: 'seat-review' });
		const tabStops = screen.getAllByRole('option').filter((o) => o.getAttribute('tabindex') === '0');
		expect(tabStops).toHaveLength(1);
	});

	it('⌥↓ in the dispatch input cycles the target to the next seat', async () => {
		await mountRail();
		fireEvent.click(screen.getByRole('option', { name: /^@lead/ }));
		const input = screen.getByRole('textbox', { name: 'Dispatch an instruction' });
		fireEvent.keyDown(input, { key: 'ArrowDown', altKey: true });
		expect(useShellStore.getState().companion.activeTarget).toEqual({ kind: 'seat', seat_id: 'seat-review' });
		fireEvent.keyDown(input, { key: 'ArrowUp', altKey: true });
		expect(useShellStore.getState().companion.activeTarget).toEqual({ kind: 'seat', seat_id: 'seat-lead' });
	});

	it('the iyke line mirrors the draft', async () => {
		await mountRail();
		fireEvent.click(screen.getByRole('option', { name: /^@lead/ }));
		fireEvent.change(screen.getByRole('textbox', { name: 'Dispatch an instruction' }), {
			target: { value: 'run the release-status check' },
		});
		expect(document.querySelector('[data-iyke-line]')?.textContent).toBe(
			'terminal-send --seat lead "run the release-status check"'
		);
	});

	it('a pending permission on @lead rides its row; other seats show where it is', async () => {
		useCompanionStore
			.getState()
			.receivePermission({ id: 'p1', kind: 'permission', toolName: 'Read', sessionId: 'term-3' });
		await mountRail();
		expect(screen.getByRole('option', { name: /^@lead/ }).querySelector('[data-signal="ask"]')).not.toBeNull();
		fireEvent.click(screen.getByRole('option', { name: /^@review/ }));
		const elsewhere = await screen.findByRole('button', { name: '1 pending on @lead' });
		fireEvent.click(elsewhere);
		expect(useShellStore.getState().companion.activeTarget).toEqual({ kind: 'seat', seat_id: 'seat-lead' });
		expect(await screen.findByRole('group', { name: 'Permission request: Read' })).toBeTruthy();
	});
});

describe('the seat menu', () => {
	it('carries D-09’s items in order; Take over only while another client holds', async () => {
		await mountRail();
		fireEvent.contextMenu(screen.getByRole('option', { name: /^@lead/ }));
		const menu = screen.getByRole('menu', { name: 'Seat actions for @lead' });
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
		fireEvent.keyDown(menu, { key: 'Escape' });
		expect(screen.queryByRole('menu')).toBeNull();

		cleanup();
		queryClient.clear();
		const held = roster();
		held[0] = { ...held[0], hold: { client: 'iyke', since: Date.now(), expires_at: Date.now() + 60_000 } };
		await mountRail(held);
		expect(screen.getByRole('option', { name: /^@lead/ }).textContent).toMatch(/held by iyke since/);
		fireEvent.contextMenu(screen.getByRole('option', { name: /^@lead/ }));
		const first = within(screen.getByRole('menu')).getAllByRole('menuitem')[0];
		expect(first.textContent).toMatch(/^Take over/);
		fireEvent.click(first);
		await waitFor(() =>
			expect(m.seatsResolve).toHaveBeenCalledWith({ seatId: 'seat-lead' }, { client: 'ui', takeover: true })
		);
	});

	it('vacant @docs: Open in pane, Pop out and End session are disabled', async () => {
		await mountRail();
		fireEvent.contextMenu(screen.getByRole('option', { name: /^@docs/ }));
		const menu = screen.getByRole('menu');
		for (const name of [/^Open in pane/, /^Pop out/, /^End session/]) {
			expect((within(menu).getByRole('menuitem', { name }) as HTMLButtonElement).disabled).toBe(true);
		}
	});

	it('Pop out calls today’s spawnWindow with the seat’s terminal (G-97)', async () => {
		await mountRail();
		fireEvent.contextMenu(screen.getByRole('option', { name: /^@lead/ }));
		fireEvent.click(screen.getByRole('menuitem', { name: /^Pop out/ }));
		await waitFor(() =>
			expect(m.spawnWindow).toHaveBeenCalledWith(
				expect.objectContaining({ kind: 'single-surface', surface_set: ['terminal:pty-term-3'] })
			)
		);
		await waitFor(() =>
			expect(useSeatNotice.getState().notice?.message).toBe('lead moved to Window 2 — its address is unchanged')
		);
	});

	it('Remove seat… confirms first, exactly as drawn', async () => {
		await mountRail();
		fireEvent.contextMenu(screen.getByRole('option', { name: /^@docs/ }));
		fireEvent.click(screen.getByRole('menuitem', { name: /^Remove seat/ }));
		const dialog = await screen.findByRole('dialog');
		expect(dialog.textContent).toContain('Its scratchpad seat:royalti-co/docs (5 entries) and its session history go with it.');
		expect(dialog.textContent).toContain('You can undo for eight seconds.');
		fireEvent.click(within(dialog).getByRole('button', { name: 'Keep it' }));
		await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
		expect(m.seatsRemove).not.toHaveBeenCalled();
	});
});

describe('Remove / Clear: the 8 s client-side Undo (§4.2)', () => {
	it('Remove calls seats_remove with removeMemory: true only when the window closes', () => {
		vi.useFakeTimers();
		const docs = roster()[3];
		removeSeat(docs, null);
		expect(useSeatUi.getState().removing['seat-docs']).toBe(true);
		expect(useSeatNotice.getState().notice?.message).toBe('Removed seat docs');
		vi.advanceTimersByTime(7_999);
		expect(m.seatsRemove).not.toHaveBeenCalled();
		vi.advanceTimersByTime(1);
		expect(m.seatsRemove).toHaveBeenCalledWith('seat-docs', { removeMemory: true }, { client: 'ui' });
	});

	it('Undo inside the window never calls the host', () => {
		vi.useFakeTimers();
		const docs = roster()[3];
		removeSeat(docs, null);
		act(() => {
			void useSeatNotice.getState().notice?.action?.run();
		});
		expect(useSeatUi.getState().removing['seat-docs']).toBeUndefined();
		vi.advanceTimersByTime(10_000);
		expect(m.seatsRemove).not.toHaveBeenCalled();
	});

	it('Clear keeps the pad and is undoable the same way', () => {
		vi.useFakeTimers();
		const docs = roster()[3];
		clearSeat(docs);
		expect(useSeatNotice.getState().notice?.message).toBe(
			'Cleared docs — session history forgotten; scratchpad seat:royalti-co/docs kept'
		);
		expect(useSeatNotice.getState().notice?.ttlMs).toBe(8_000);
		vi.advanceTimersByTime(8_000);
		expect(m.seatsClear).toHaveBeenCalledWith('seat-docs', { client: 'ui' });
	});
});

describe('vacant (D-09 vacant state)', () => {
	it('shows the last session and three ways on', async () => {
		await mountRail();
		fireEvent.click(screen.getByRole('option', { name: /^@docs/ }));
		const panel = await waitFor(() => {
			const el = document.querySelector('[data-state="seats-vacant"]') as HTMLElement | null;
			expect(el).not.toBeNull();
			return el as HTMLElement;
		});
		expect(panel.textContent).toContain(`session ${sessionNumber('term-2')} · claude-code`);
		expect(panel.textContent).toContain('5 entries · “STATUS.md pass half done”');
		expect(within(panel).getByRole('button', { name: `Resume session ${sessionNumber('term-2')}` })).toBeTruthy();
		expect(within(panel).getByRole('button', { name: 'Fill with a new session' })).toBeTruthy();
		expect(within(panel).getByRole('button', { name: 'Clear seat' })).toBeTruthy();
	});

	it('an explicit Resume never falls back: disabled with the reason (§6.2)', async () => {
		const seats = roster();
		seats[3] = { ...seats[3], engine_resume: 'process-local', resume: { resumable: false, reason: 'process_local' } };
		await mountRail(seats);
		fireEvent.click(screen.getByRole('option', { name: /^@docs/ }));
		const resume = await screen.findByRole('button', { name: /^Resume session/ });
		expect((resume as HTMLButtonElement).disabled).toBe(true);
		expect(resume.getAttribute('title')).toBe('not resumable after restart');
		// The flag is on the row at all times, not only when vacant.
		expect(screen.getByRole('option', { name: /^@docs/ }).textContent).toContain('not resumable after restart');
	});
});

describe('empty + create (D-09 empty / create states)', () => {
	it('empty: one sentence, New seat and Seat this session…', async () => {
		await mountRail([]);
		const empty = document.querySelector('[data-state="seats-empty"]') as HTMLElement;
		expect(empty.textContent).toContain('A seat keeps an agent’s name when its pane moves or its session ends.');
		expect(within(empty).getByRole('button', { name: 'New seat' })).toBeTruthy();
		fireEvent.click(within(empty).getByRole('button', { name: 'Seat this session…' }));
		const form = await waitFor(() => {
			const el = document.querySelector('[data-state="seats-create"]');
			expect(el).not.toBeNull();
			return el as HTMLElement;
		});
		// Seating an open session locks the engine and picks "an open session".
		expect(within(form).getByRole('radio', { name: /an open session/ }).getAttribute('aria-checked')).toBe('true');
	});

	it('create: live validation, the engine list, the canonical scratchpad and the iyke line', async () => {
		await mountRail();
		fireEvent.click(screen.getByRole('button', { name: 'New seat' }));
		const form = document.querySelector('[data-state="seats-create"]') as HTMLElement;
		const name = within(form).getByRole('textbox');
		fireEvent.change(name, { target: { value: 'review' } });
		expect(form.textContent).toContain('review is already a seat');
		expect((within(form).getByRole('button', { name: 'Create seat' }) as HTMLButtonElement).disabled).toBe(true);
		fireEvent.change(name, { target: { value: 'scribe' } });
		expect(form.textContent).toContain('@scribe is free in royalti-co');
		expect(form.querySelector('[data-seat-pad-preview]')?.textContent).toBe('seat:royalti-co/scribe');
		expect(form.querySelector('[data-seat-form-iyke]')?.textContent).toBe('seat create scribe --engine claude-code');
		const gemini = await within(form).findByRole('radio', { name: 'gemini' });
		expect((gemini as HTMLButtonElement).disabled).toBe(true);
		expect(gemini.getAttribute('title')).toBe('not installed — Ngwa → Store');
		// resume a past session → the exact --resume flag.
		fireEvent.click(within(form).getByRole('radio', { name: /resume a past session/ }));
		expect(form.querySelector('[data-seat-form-iyke]')?.textContent).toBe(
			'seat create scribe --engine claude-code --resume term-2'
		);
		fireEvent.keyDown(name, { key: 'Escape' });
		await waitFor(() => expect(document.querySelector('[data-state="seats-create"]')).toBeNull());
	});
});

describe('dispatch (D-09 dispatch state)', () => {
	it('the picker lists seats first, then unseated, then new session / persistent run', async () => {
		await mountRail();
		fireEvent.click(screen.getByRole('button', { name: /^Dispatch target:/ }));
		const menu = await screen.findByRole('menu', { name: 'Dispatch targets' });
		expect(menu.getAttribute('data-state')).toBe('seats-dispatch');
		const groups = within(menu)
			.getAllByRole('group')
			.map((g) => g.getAttribute('aria-label'));
		expect(groups.slice(0, 2)).toEqual(['Seats', 'Unseated']);
		expect(groups).toContain('New session on…');
		expect(groups).toContain('Persistent run');
		const seats = within(within(menu).getAllByRole('group')[0]).getAllByRole('menuitemradio');
		expect(seats.map((s) => s.textContent?.split(/claude|codex|run|vacant/)[0])).toEqual([
			'@lead',
			'@review',
			'@nightly',
			'@docs',
		]);
	});
});

describe('rest (D-09 rest state)', () => {
	it('the 36 px strip keeps the seat signals: a permission on @lead, the run pulse on @nightly', async () => {
		m.seatsList.mockResolvedValue(roster());
		useCompanionStore
			.getState()
			.receivePermission({ id: 'p1', kind: 'permission', toolName: 'Read', sessionId: 'term-3' });
		useCompanionStore.setState({ state: 'collapsed' });
		wrap(<Companion />);
		const strip = await waitFor(() => {
			const el = document.querySelector('[data-state="seats-rest"]') as HTMLElement | null;
			expect(el?.querySelector('[data-mono="lead"]')).not.toBeNull();
			return el as HTMLElement;
		});
		const lead = strip.querySelector('[data-mono="lead"]') as HTMLElement;
		expect(lead.querySelector('[data-attention="permission"]')?.textContent).toBe('1');
		const nightly = strip.querySelector('[data-mono="nightly"]') as HTMLElement;
		expect(nightly.querySelector('[data-dot="run"]')?.className).toContain('animate-pulse');
		// Clicking a monogram expands on that seat.
		fireEvent.click(nightly);
		expect(useCompanionStore.getState().state).toBe('expanded');
		expect(useShellStore.getState().companion.activeTarget).toEqual({ kind: 'seat', seat_id: 'seat-nightly' });
	});
});
