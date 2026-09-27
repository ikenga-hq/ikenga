// WP-67 — the seat rail's words and signals (D-09 `seats-companion.html`,
// G-SEATS §1.2, §5.2, §6.2, §6a, §7.3).

import { describe, expect, it } from 'vitest';
import type { SeatView } from '@/lib/tauri-cmd';
import {
	atName,
	checkSeatName,
	costLine,
	ctxK,
	engineResumeFlag,
	engineShort,
	heldByOther,
	holdText,
	iykeChiRun,
	iykeSeatCreate,
	iykeSendToSeat,
	iykeVacant,
	notResumableText,
	padText,
	seatChipRest,
	seatMonogram,
	seatScope,
	seatSendHint,
	seatSessionRef,
	seatWhoLine,
	usd,
} from './seat-model';

function seat(over: Partial<SeatView> = {}): SeatView {
	return {
		id: 'seat-1',
		project_id: 'royalti-co',
		name: 'lead',
		engine_id: 'claude-code',
		session: { kind: 'terminal', terminal_id: 'term-3', external_id: 'conv-1', cwd: '/w' },
		created_at: 0,
		last_active_at: 0,
		hold: null,
		address: 'seat:royalti-co/lead',
		agent_id: 'seat-1',
		status: 'live',
		agent: 'live',
		resume: { resumable: true },
		engine_resume: 'durable',
		mount: null,
		queued: null,
		pad: { count: 3, latest: { name: 'WP-64 brief drafted', updated_at: 0 } },
		inbox_count: 0,
		...over,
	};
}

describe('checkSeatName (§1.2, D-09 create)', () => {
	const taken = ['lead', 'review', 'nightly', 'docs'];
	it('empty is neither ok nor an error', () => {
		expect(checkSeatName('', taken)).toEqual({ ok: false, empty: true, message: '' });
	});
	it('a taken name reads "<name> is already a seat"', () => {
		expect(checkSeatName('review', taken)).toMatchObject({ ok: false, message: 'review is already a seat' });
	});
	it('the charset, then the hyphen rule, then the length', () => {
		expect(checkSeatName('Review', taken).message).toBe('Use a–z, 0–9 and - only');
		expect(checkSeatName('my seat', taken).message).toBe('Use a–z, 0–9 and - only');
		expect(checkSeatName('-a', taken).message).toBe('A name can’t start or end with -');
		expect(checkSeatName('a-', taken).message).toBe('A name can’t start or end with -');
		expect(checkSeatName('a'.repeat(33), taken).message).toBe('At most 32 characters');
	});
	it('a free name says where it is free', () => {
		expect(checkSeatName('scribe', taken, { project: 'royalti-co' })).toEqual({
			ok: true,
			message: '@scribe is free in royalti-co',
		});
		expect(checkSeatName('a', taken).ok).toBe(true);
		expect(checkSeatName('a'.repeat(32), taken).ok).toBe(true);
	});
	it('a rename may keep its own name', () => {
		expect(checkSeatName('lead', taken, { except: 'lead' }).ok).toBe(true);
		expect(checkSeatName('review', taken, { except: 'lead' }).ok).toBe(false);
	});
	it('a seat inside its Remove window still holds its name', () => {
		expect(checkSeatName('old', taken, { removing: ['old'] })).toMatchObject({
			ok: false,
			message: 'old is being removed — Undo it or wait 8 s',
		});
		expect(checkSeatName('new', taken, { removing: ['old'] }).ok).toBe(true);
	});
});

describe('addresses (§1.3, §6a)', () => {
	it('the scratchpad scope is canonical — never the mockup’s seat:<name>', () => {
		expect(seatScope('royalti-co', 'review')).toBe('seat:royalti-co/review');
	});
	it('@name and the two-letter monogram', () => {
		expect(atName('lead')).toBe('@lead');
		expect(seatMonogram('nightly')).toBe('ni');
	});
});

describe('rows and the chip', () => {
	it('who line: engine · session N, idle, run, vacant', () => {
		expect(seatWhoLine(seat(), 'session 3')).toBe('claude-code · session 3');
		expect(seatWhoLine(seat({ status: 'idle' }), 'session 1')).toBe('claude-code · session 1 · idle');
		expect(seatWhoLine(seat({ status: 'run' }), 'session 5')).toBe('run · session 5');
		expect(seatWhoLine(seat({ status: 'vacant' }), 'session 2')).toBe('vacant · session 2 ended');
		expect(seatWhoLine(seat({ status: 'vacant', session: null }), null)).toBe('vacant · no history');
	});
	it('the chip reads @lead · claude · session 3 (D-09 rule 5)', () => {
		expect(`${atName('lead')}${seatChipRest(seat(), 'session 3')}`).toBe('@lead · claude · session 3');
		expect(seatChipRest(seat({ status: 'vacant' }), 'session 2')).toBe(' · vacant · resumes session 2');
		expect(
			seatChipRest(seat({ status: 'vacant', resume: { resumable: false, reason: 'no_session' }, session: null }), null)
		).toBe(' · vacant · fills on send');
	});
	it('the hint says what ↵ does', () => {
		expect(seatSendHint(seat(), 'session 3')).toBe('send to @lead');
		expect(seatSendHint(seat({ status: 'vacant', name: 'docs' }), 'session 2')).toBe('resume session 2, then send');
		expect(
			seatSendHint(seat({ status: 'vacant', name: 'x', session: null, resume: { resumable: false, reason: 'no_session' } }), null)
		).toBe('fill @x, then send');
		expect(seatSendHint(seat({ status: 'run', name: 'nightly' }), 'session 4')).toBe('resume the run in @nightly');
	});
	it('the pad line, with the inbox only while occupied', () => {
		expect(padText(seat())).toBe('3 entries · “WP-64 brief drafted”');
		expect(padText(seat({ pad: { count: 1, latest: null }, inbox_count: 1 }))).toBe('1 entry · inbox 1');
		expect(padText(seat({ pad: { count: 1, latest: null }, inbox_count: 1, status: 'vacant' }))).toBe('1 entry');
	});
	it('the panels scope to the seat’s session ref (§9.1)', () => {
		expect(seatSessionRef(seat())).toBe('term-3');
		expect(seatSessionRef(seat({ session: { kind: 'run', run_id: 'run-9', external_id: null, cwd: null } }))).toBe('run-9');
		expect(seatSessionRef(seat({ session: null }))).toBeNull();
	});
	it('engine short names', () => {
		expect(engineShort('claude-code')).toBe('claude');
		expect(engineShort('codex')).toBe('codex');
	});
});

describe('engine flags (§6.2, P-5)', () => {
	it('carried at all times by process-local and none engines', () => {
		expect(engineResumeFlag('process-local')).toBe('not resumable after restart');
		expect(engineResumeFlag('none')).toBe('can’t resume sessions');
		expect(engineResumeFlag('durable')).toBeNull();
	});
	it('every not-resumable reason has words', () => {
		for (const r of [
			'no_session',
			'process_local',
			'no_resume_support',
			'no_resume_id',
			'run_missing',
			'engine_unavailable',
		] as const) {
			expect(notResumableText(r)).toBeTruthy();
		}
		expect(notResumableText('process_local')).toBe('not resumable after restart');
	});
});

describe('holds (§5.2, §5.5)', () => {
	const fmt = () => '09:40';
	const now = 1_000_000;
	it('"held by X since T" only while unexpired', () => {
		expect(holdText({ client: 'iyke', since: 1, expires_at: now + 1 }, fmt, now)).toBe('held by iyke since 09:40');
		expect(holdText({ client: 'iyke', since: 1, expires_at: now - 1 }, fmt, now)).toBeNull();
		expect(holdText(null, fmt, now)).toBeNull();
	});
	it('Take over shows only while ANOTHER client holds', () => {
		expect(heldByOther({ client: 'iyke', since: 1, expires_at: now + 1 }, 'ui', now)).toBe(true);
		expect(heldByOther({ client: 'ui', since: 1, expires_at: now + 1 }, 'ui', now)).toBe(false);
		expect(heldByOther({ client: 'iyke', since: 1, expires_at: now - 1 }, 'ui', now)).toBe(false);
		expect(heldByOther(null, 'ui', now)).toBe(false);
	});
});

describe('figures (D-09 revision 2, G-93)', () => {
	it('only reported figures; nothing is invented', () => {
		expect(ctxK(38120)).toBe('38k');
		expect(ctxK(undefined)).toBeNull();
		expect(ctxK(0)).toBeNull();
		expect(usd(1.4213)).toBe('$1.42');
		expect(usd(undefined)).toBeNull();
	});
	it('never "— — ctx": one dash when nothing is reported', () => {
		expect(costLine({ amt: null, ctx: null })).toEqual({ text: '—', unreported: true });
		expect(costLine({ amt: '$1.42', ctx: '38k' })).toEqual({ text: '$1.42 · 38k ctx', unreported: false });
		expect(costLine({ amt: '$0.10', ctx: null })).toEqual({ text: '$0.10', unreported: true });
		expect(costLine({ amt: null, ctx: '2k' })).toEqual({ text: '— · 2k ctx', unreported: true });
	});
});

describe('the iyke form (Principle 5, §7.3)', () => {
	it('mirrors the draft', () => {
		expect(iykeSendToSeat('lead', 'run the release-status check')).toBe(
			'terminal-send --seat lead "run the release-status check"'
		);
		expect(iykeSendToSeat('lead', '  ')).toBe('terminal-send --seat lead "…"');
		expect(iykeChiRun('codex', true, 'x')).toBe('chi run codex --persistent --prompt "x"');
	});
	it('the create form uses the exact flags: --engine, --session, --resume', () => {
		expect(iykeSeatCreate('review', 'codex', { kind: 'new' })).toBe('seat create review --engine codex');
		expect(iykeSeatCreate('helper', 'claude-code', { kind: 'open', ref: 'term-4' })).toBe(
			'seat create helper --session term-4'
		);
		expect(iykeSeatCreate('scribe', 'claude-code', { kind: 'resume', ref: 'term-2' })).toBe(
			'seat create scribe --engine claude-code --resume term-2'
		);
		expect(iykeSeatCreate('', 'claude-code', { kind: 'new' })).toBe('seat create <name> --engine claude-code');
	});
	it('the vacant panel line carries the prompt the bridge needs (§7.2 needs_prompt)', () => {
		expect(iykeVacant('docs', true)).toBe('seat resume docs --prompt "…"');
		expect(iykeVacant('docs', false)).toBe('seat fill docs --prompt "…"');
	});
});
