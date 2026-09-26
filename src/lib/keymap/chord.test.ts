import { describe, expect, it, vi } from 'vitest';
import {
	CHORD_TIMEOUT_MS,
	type ChordBinding,
	ChordMachine,
	type ChordOutcome,
	chordPrefixDelays,
	chordPrefixFor,
	ownerHasFocus,
} from './chord';

const PALETTE: ChordBinding = { key: 'mod+k', command: 'palette.open', when: '!inputFocus && !paletteOpen' };
const RELEASE: ChordBinding = { key: 'mod+k mod+r', command: 'release-status' };
const FILES_CHORD: ChordBinding = { key: 'mod+k mod+e', command: 'explain-file', when: 'filesFocus' };

/** A machine with a hand-driven timer. */
function makeMachine(bindings: ChordBinding[], mac = true) {
	let timerFn: (() => void) | null = null;
	let timerMs: number | null = null;
	const cleared: unknown[] = [];
	const timeouts: Array<Extract<ChordOutcome<string>, { type: 'timeout' }>> = [];
	const machine = new ChordMachine<string>({
		getBindings: () => bindings,
		mac,
		onTimeout: (o) => timeouts.push(o),
		setTimer: (fn, ms) => {
			timerFn = fn;
			timerMs = ms;
			return 'handle';
		},
		clearTimer: (h) => {
			cleared.push(h);
			timerFn = null;
		},
	});
	return {
		machine,
		timeouts,
		cleared,
		fireTimer: () => timerFn?.(),
		get timerMs() {
			return timerMs;
		},
	};
}

describe('chord gating (DEC-57, §3.2)', () => {
	it('does not enter chord mode when no chord with that prefix is bound — ⌘K resolves at once', () => {
		const { machine } = makeMachine([PALETTE]);
		expect(machine.press(['meta+k'], {})).toEqual({ type: 'none' });
		expect(machine.isPending).toBe(false);
	});

	it('enters chord mode while a chord with that prefix is bound, with a 900 ms timer', () => {
		const t = makeMachine([PALETTE, RELEASE]);
		expect(t.machine.press(['meta+k'], {}, 'palette.open')).toEqual({ type: 'pending', first: 'meta+k' });
		expect(t.machine.isPending).toBe(true);
		expect(t.machine.pendingPrefix).toBe('meta+k');
		expect(t.timerMs).toBe(CHORD_TIMEOUT_MS);
		expect(CHORD_TIMEOUT_MS).toBe(900);
	});

	it('is "bound" whatever the chord `when` (literal DEC-57, B-10)', () => {
		const { machine } = makeMachine([PALETTE, FILES_CHORD]);
		expect(machine.press(['meta+k'], { filesFocus: false }).type).toBe('pending');
	});

	it('resolves `mod` per platform', () => {
		const other = makeMachine([PALETTE, RELEASE], false);
		expect(other.machine.press(['meta+k'], {}).type).toBe('none');
		expect(other.machine.press(['ctrl+k'], {}).type).toBe('pending');
	});

	it('ignores single-stroke bindings when looking for prefixes', () => {
		expect(chordPrefixFor(['meta+k'], [PALETTE], { mac: true, ctx: {} })).toBeNull();
	});
});

describe('owner carve-out (§3.2, B-10)', () => {
	it('recognises the three owner conditions', () => {
		expect(ownerHasFocus({ terminalFocus: true })).toBe(true);
		expect(ownerHasFocus({ dispatchFocus: true })).toBe(true);
		expect(ownerHasFocus({ paletteOpen: true })).toBe(true);
		expect(ownerHasFocus({ filesFocus: true })).toBe(false);
	});

	it('with the terminal focused, a bound chord whose `when` is false does not hold the stroke back', () => {
		const { machine } = makeMachine([PALETTE, FILES_CHORD], false);
		expect(machine.press(['ctrl+k'], { terminalFocus: true, filesFocus: false })).toEqual({ type: 'none' });
	});

	it('with the terminal focused, a chord whose `when` is true now still enters chord mode', () => {
		const { machine } = makeMachine([PALETTE, RELEASE], false);
		expect(machine.press(['ctrl+k'], { terminalFocus: true }).type).toBe('pending');
	});

	it('with the palette open, ⌘K is not held back by a chord that cannot fire', () => {
		const { machine } = makeMachine([PALETTE, FILES_CHORD]);
		expect(machine.press(['meta+k'], { paletteOpen: true })).toEqual({ type: 'none' });
	});

	it('with the dispatch input focused, the same rule applies', () => {
		const { machine } = makeMachine([PALETTE, FILES_CHORD]);
		expect(machine.press(['meta+k'], { dispatchFocus: true })).toEqual({ type: 'none' });
		expect(machine.press(['meta+k'], { dispatchFocus: true, filesFocus: true }).type).toBe('pending');
	});
});

describe('chord mode outcomes', () => {
	it('a completing second stroke whose `when` is true fires the chord', () => {
		const t = makeMachine([PALETTE, RELEASE]);
		t.machine.press(['meta+k'], {}, 'palette.open');
		const out = t.machine.press(['meta+r'], {});
		expect(out.type).toBe('chord');
		if (out.type === 'chord') {
			expect(out.candidates.map((c) => c.command)).toEqual(['release-status']);
			expect(out.first).toBe('meta+k');
			expect(out.second).toBe('meta+r');
		}
		expect(t.machine.isPending).toBe(false);
		expect(t.cleared).toContain('handle');
		t.fireTimer();
		expect(t.timeouts).toEqual([]);
	});

	it('900 ms elapse → the first stroke\'s single-stroke winner fires (timeout)', () => {
		const t = makeMachine([PALETTE, RELEASE]);
		t.machine.press(['meta+k'], {}, 'palette.open');
		t.fireTimer();
		expect(t.timeouts).toEqual([{ type: 'timeout', first: 'meta+k', winner: 'palette.open' }]);
		expect(t.machine.isPending).toBe(false);
	});

	it('a second stroke that completes no chord → fallthrough with the winner, then idle', () => {
		const t = makeMachine([PALETTE, RELEASE]);
		t.machine.press(['meta+k'], {}, 'palette.open');
		expect(t.machine.press(['meta+x'], {})).toEqual({ type: 'fallthrough', first: 'meta+k', winner: 'palette.open' });
		expect(t.machine.isPending).toBe(false);
		// The dispatcher re-dispatches meta+x as a fresh stroke: not a prefix.
		expect(t.machine.press(['meta+x'], {})).toEqual({ type: 'none' });
	});

	it('a completing stroke whose chord `when` is false is a fallthrough', () => {
		const t = makeMachine([PALETTE, FILES_CHORD]);
		t.machine.press(['meta+k'], { filesFocus: false }, 'palette.open');
		expect(t.machine.press(['meta+e'], { filesFocus: false }).type).toBe('fallthrough');
	});

	it('escape ends chord mode and nothing fires', () => {
		const t = makeMachine([PALETTE, RELEASE]);
		t.machine.press(['meta+k'], {}, 'palette.open');
		expect(t.machine.press(['escape'], {})).toEqual({ type: 'cancelled', first: 'meta+k' });
		t.fireTimer();
		expect(t.timeouts).toEqual([]);
	});

	it('cancel() leaves chord mode without firing', () => {
		const t = makeMachine([PALETTE, RELEASE]);
		t.machine.press(['meta+k'], {}, 'palette.open');
		t.machine.cancel();
		expect(t.machine.isPending).toBe(false);
		t.fireTimer();
		expect(t.timeouts).toEqual([]);
	});

	it('matches any candidate stroke of the second event', () => {
		const t = makeMachine([PALETTE, { key: 'mod+k ?', command: 'help.chord' }]);
		t.machine.press(['meta+k'], {});
		const out = t.machine.press(['?', 'shift+/'], {});
		expect(out.type).toBe('chord');
	});

	it('reads bindings on every press, so a re-merge applies without rebuilding', () => {
		const bindings: ChordBinding[] = [PALETTE];
		const machine = new ChordMachine({ getBindings: () => bindings, mac: true, onTimeout: vi.fn() });
		expect(machine.press(['meta+k'], {}).type).toBe('none');
		bindings.push(RELEASE);
		expect(machine.press(['meta+k'], {}).type).toBe('pending');
		machine.cancel();
	});

	it('uses real timers by default', () => {
		vi.useFakeTimers();
		try {
			const onTimeout = vi.fn();
			const machine = new ChordMachine({ getBindings: () => [PALETTE, RELEASE], mac: true, onTimeout });
			machine.press(['meta+k'], {}, 'palette.open');
			vi.advanceTimersByTime(CHORD_TIMEOUT_MS - 1);
			expect(onTimeout).not.toHaveBeenCalled();
			vi.advanceTimersByTime(1);
			expect(onTimeout).toHaveBeenCalledWith({ type: 'timeout', first: 'meta+k', winner: 'palette.open' });
		} finally {
			vi.useRealTimers();
		}
	});
});

describe('chordPrefixDelays — the Keys-tab palette-delay warning', () => {
	it('flags every single-stroke binding a chord makes a prefix', () => {
		const rows = chordPrefixDelays([PALETTE, RELEASE, { key: 'mod+j', command: 'companion.toggle' }], true);
		expect(rows.map((r) => r.binding.command)).toEqual(['palette.open']);
		expect(rows[0].chords.map((c) => c.command)).toEqual(['release-status']);
	});

	it('is empty when no chord is bound', () => {
		expect(chordPrefixDelays([PALETTE], true)).toEqual([]);
	});
});
