// WP-68 — the seat board's pure readouts (D-09 `seats-board.html`).

import { describe, expect, it } from 'vitest';
import type { SeatView } from '@/lib/tauri-cmd';
import type { UnseatedSession } from '@/shell/companion/seat-roster';
import {
	boardIyke,
	boardRows,
	boardState,
	countLine,
	mountSignature,
	mountText,
	movedToWindow,
	moveSelection,
	padLatest,
	relativeTime,
	rowKey,
	stateWord,
	validSelection,
} from './board-model';

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
		last_active_at: 0,
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

const lead = seat({
	name: 'lead',
	status: 'live',
	session: { kind: 'terminal', terminal_id: 'term-3', external_id: 'c3', cwd: '/w' },
	pad: { count: 3, latest: { name: 'WP-64 brief drafted', updated_at: 0 } },
});
const docs = seat({
	name: 'docs',
	status: 'vacant',
	session: { kind: 'terminal', terminal_id: 'term-2', external_id: 'c2', cwd: '/w' },
	resume: { resumable: true },
});
const cleared = seat({ name: 'fresh' });
const s4: UnseatedSession = {
	id: 'term-4',
	engineId: 'claude-code',
	parkedIdx: null,
	status: 'running',
	title: 'term-4',
	cwd: '/w',
	externalId: null,
};

describe('rows and selection', () => {
	it('lists every seat, then the unseated sessions', () => {
		const rows = boardRows([lead, docs], [s4]);
		expect(rows.map(rowKey)).toEqual(['seat:seat-lead', 'seat:seat-docs', 'session:term-4']);
	});

	it('a selection that no longer names a row is dropped (no detail column)', () => {
		expect(validSelection({ kind: 'seat', id: 'seat-lead' }, [lead], [])).toEqual({ kind: 'seat', id: 'seat-lead' });
		expect(validSelection({ kind: 'seat', id: 'seat-gone' }, [lead], [])).toBeNull();
		expect(validSelection({ kind: 'session', id: 'term-4' }, [lead], [s4])).toEqual({ kind: 'session', id: 'term-4' });
		expect(validSelection(null, [lead], [s4])).toBeNull();
	});

	it('↑ ↓ Home End rove without wrapping; from nothing they start at the top', () => {
		const rows = boardRows([lead, docs], [s4]);
		expect(moveSelection(rows, { kind: 'seat', id: 'seat-lead' }, 'ArrowDown')).toEqual({ kind: 'seat', id: 'seat-docs' });
		expect(moveSelection(rows, { kind: 'seat', id: 'seat-docs' }, 'ArrowDown')).toEqual({ kind: 'session', id: 'term-4' });
		expect(moveSelection(rows, { kind: 'session', id: 'term-4' }, 'ArrowDown')).toEqual({ kind: 'session', id: 'term-4' });
		expect(moveSelection(rows, { kind: 'seat', id: 'seat-lead' }, 'ArrowUp')).toEqual({ kind: 'seat', id: 'seat-lead' });
		expect(moveSelection(rows, { kind: 'seat', id: 'seat-docs' }, 'End')).toEqual({ kind: 'session', id: 'term-4' });
		expect(moveSelection(rows, { kind: 'session', id: 'term-4' }, 'Home')).toEqual({ kind: 'seat', id: 'seat-lead' });
		expect(moveSelection(rows, null, 'ArrowDown')).toEqual({ kind: 'seat', id: 'seat-lead' });
		expect(moveSelection([], null, 'Home')).toBeNull();
	});
});

describe('readouts', () => {
	it('the head counts seats, vacant seats and unseated sessions (D-09 wording)', () => {
		const review = seat({ name: 'review', status: 'idle' });
		const nightly = seat({ name: 'nightly', status: 'run' });
		expect(countLine([lead, review, nightly, docs], 1)).toBe('4 seats · 1 vacant · 1 unseated session');
		expect(countLine([], 3)).toBe('no seats · 3 unseated sessions');
		expect(countLine([lead], 0)).toBe('1 seat · 0 unseated sessions');
	});

	it('state words carry no invented duration', () => {
		expect(stateWord({ status: 'idle', agent: 'live' })).toBe('idle');
		expect(stateWord({ status: 'run', agent: null })).toBe('run');
		expect(stateWord({ status: 'vacant', agent: null })).toBe('vacant');
		expect(stateWord({ status: 'live', agent: 'starting' })).toBe('live · starting');
		expect(stateWord({ status: 'live', agent: 'unreported' })).toBe('live');
	});

	it('mounts read short in a normal pane and long in a wide one; a vacant seat has none', () => {
		expect(mountText({ where: 'main', paneIndex: 1, leafId: 'L1' }, { isRun: false, vacant: false })).toEqual({
			short: 'pane 1',
			long: 'main window · pane 1',
			where: 'main',
		});
		expect(mountText({ where: 'window', label: 'w2' }, { isRun: false, vacant: false }).short).toBe('Window 2');
		expect(mountText({ where: 'none' }, { isRun: true, vacant: false })).toEqual({
			short: 'headless',
			long: 'not mounted (headless)',
			where: 'headless',
		});
		expect(mountText({ where: 'none' }, { isRun: false, vacant: false }).short).toBe('not mounted');
		expect(mountText({ where: 'main', paneIndex: 2, leafId: 'L2' }, { isRun: false, vacant: true }).where).toBe(
			'vacant'
		);
	});

	it('"moved" is a change INTO a window, never the first sighting (G-93, G-96)', () => {
		const main = mountSignature({ where: 'main', paneIndex: 1, leafId: 'L1' });
		const w2 = mountSignature({ where: 'window', label: 'detached-1' });
		expect(movedToWindow(main, w2)).toBe(true);
		expect(movedToWindow('none', w2)).toBe(true);
		expect(movedToWindow(undefined, w2)).toBe(false);
		expect(movedToWindow(w2, w2)).toBe(false);
		expect(movedToWindow(w2, main)).toBe(false);
	});

	it('the scratchpad cell: latest entry and count, or empty', () => {
		expect(padLatest(lead)).toEqual({ latest: '“WP-64 brief drafted”', count: '3 entries' });
		expect(padLatest(cleared)).toEqual({ latest: null, count: 'empty' });
		expect(padLatest(seat({ pad: { count: 1, latest: null } }))).toEqual({ latest: null, count: '1 entry' });
	});

	it('relative times', () => {
		const now = 10 * 3600_000;
		expect(relativeTime(now - 20_000, now)).toBe('just now');
		expect(relativeTime(now - 4 * 60_000, now)).toBe('4m ago');
		expect(relativeTime(now - 2 * 3600_000, now)).toBe('2h ago');
	});
});

describe('the iyke line (Principle 5, G-SEATS §7.3)', () => {
	it('names the selected row the way a caller would address it', () => {
		expect(boardIyke(null, PROJECT)).toBe('seat ls --project royalti-co');
		expect(boardIyke({ kind: 'seat', seat: lead }, PROJECT)).toBe('terminal-send --seat lead "…"');
		expect(boardIyke({ kind: 'seat', seat: docs }, PROJECT)).toBe('seat resume docs --prompt "…"');
		expect(boardIyke({ kind: 'seat', seat: cleared }, PROJECT)).toBe('seat fill fresh --prompt "…"');
		expect(boardIyke({ kind: 'session', session: s4 }, PROJECT)).toBe('seat create <name> --session term-4');
	});
});

describe('state map (G-55)', () => {
	const base = { state: 'ready' as const, seatCount: 4, formOpen: false, moving: false, selectedVacant: false };
	it('one data-state per D-09 state the board owns', () => {
		expect(boardState(base)).toBe('board-roster');
		expect(boardState({ ...base, seatCount: 0 })).toBe('board-empty');
		expect(boardState({ ...base, formOpen: true })).toBe('board-create');
		expect(boardState({ ...base, selectedVacant: true })).toBe('board-vacant');
		expect(boardState({ ...base, moving: true, selectedVacant: true })).toBe('board-popout');
		expect(boardState({ ...base, state: 'loading' })).toBe('board-loading');
		expect(boardState({ ...base, state: 'error' })).toBe('board-error');
	});
});
