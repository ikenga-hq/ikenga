// Chord mode (G-ACTIONS §3.2, DEC-57) — a pure state machine the one
// dispatcher (WP-54) drives. It never fires a command itself: every call
// returns an outcome and the dispatcher acts on it (fire, preventDefault,
// re-dispatch). Time is injectable so tests run without real timers.
//
// Rules implemented here:
// - A chord is exactly two strokes; the second must arrive within 900 ms.
// - Chord mode is entered on a stroke only while the effective keymap (for
//   this platform) holds at least one chord whose first stroke is that
//   stroke — **whatever that chord's `when`** (literal DEC-57, B-10). With
//   none bound the stroke resolves at once (⌘K opens the palette with no
//   delay).
// - **Owner carve-out**: while a hosted owner has focus (`terminalFocus`,
//   `dispatchFocus`) or the palette is open (`paletteOpen`), a stroke enters
//   chord mode only if at least one chord with that first stroke has a
//   `when` that is true *now*. Otherwise the stroke is not held back — in
//   the terminal Ctrl+K still reaches readline, and with the palette open ⌘K
//   runs `palette.close` at once.
// - In chord mode: a completing second stroke whose chord `when` is true →
//   `chord`; 900 ms elapse → `timeout` (the dispatcher fires the first
//   stroke's single-stroke winner, resolved at the first keypress); a second
//   stroke completing no chord → `fallthrough` (fire that winner, then
//   dispatch the second stroke as a fresh stroke); `escape` → `cancelled`,
//   nothing fires.

import { resolveCombo, splitKeySequence } from './platform';
import { type EvalOptions, evaluateWhen, type WhenContext } from './when';

export const CHORD_TIMEOUT_MS = 900;

/** One effective binding, already filtered to the current platform. Only
 *  two-stroke `key`s matter to the machine; single strokes are ignored. */
export interface ChordBinding {
	key: string;
	command: string;
	when?: string;
}

function resolvedStrokes(b: ChordBinding, mac: boolean): string[] {
	return splitKeySequence(b.key).map((s) => resolveCombo(s, mac));
}

/** Chords grouped by their resolved first stroke. */
export function chordsByPrefix<B extends ChordBinding>(bindings: readonly B[], mac: boolean): Map<string, B[]> {
	const out = new Map<string, B[]>();
	for (const b of bindings) {
		const strokes = resolvedStrokes(b, mac);
		if (strokes.length !== 2) continue;
		const list = out.get(strokes[0]);
		if (list) list.push(b);
		else out.set(strokes[0], [b]);
	}
	return out;
}

/** Does a hosted owner (terminal, dispatch input) have focus, or is the
 *  palette open? — the condition of the §3.2 owner carve-out. */
export function ownerHasFocus(ctx: WhenContext): boolean {
	return ctx.terminalFocus === true || ctx.dispatchFocus === true || ctx.paletteOpen === true;
}

/**
 * Which of an event's candidate strokes (see `strokesFromEvent`) enters
 * chord mode now, or `null`. Encodes the "bound" rule and the owner
 * carve-out above.
 */
export function chordPrefixFor(
	strokes: readonly string[],
	bindings: readonly ChordBinding[],
	opts: { mac: boolean; ctx: WhenContext; evalOpts?: EvalOptions }
): string | null {
	const byPrefix = chordsByPrefix(bindings, opts.mac);
	const carveOut = ownerHasFocus(opts.ctx);
	for (const stroke of strokes) {
		const chords = byPrefix.get(stroke);
		if (!chords || chords.length === 0) continue;
		if (!carveOut) return stroke;
		if (chords.some((c) => evaluateWhen(c.when, opts.ctx, opts.evalOpts))) return stroke;
	}
	return null;
}

/**
 * Single-stroke bindings that some chord makes a chord prefix — the rows the
 * Keys tab marks with the palette-delay warning (DEC-57, WP-60). Returns one
 * entry per such single-stroke binding, with the chords that cause it.
 */
export function chordPrefixDelays<B extends ChordBinding>(
	bindings: readonly B[],
	mac: boolean
): Array<{ binding: B; chords: B[] }> {
	const byPrefix = chordsByPrefix(bindings, mac);
	const out: Array<{ binding: B; chords: B[] }> = [];
	for (const b of bindings) {
		const strokes = resolvedStrokes(b, mac);
		if (strokes.length !== 1) continue;
		const chords = byPrefix.get(strokes[0]);
		if (chords && chords.length > 0) out.push({ binding: b, chords });
	}
	return out;
}

export type ChordOutcome<T> =
	/** Not in chord mode and this stroke does not enter it: resolve it as a
	 *  single stroke (§2.3). */
	| { type: 'none' }
	/** Chord mode entered: swallow the event (preventDefault), fire nothing. */
	| { type: 'pending'; first: string }
	/** The second stroke completed one or more chords whose `when` is true
	 *  now; the dispatcher picks the winner among `candidates` by §2.3. */
	| { type: 'chord'; first: string; second: string; candidates: ChordBinding[] }
	/** The second stroke completed no chord: fire `winner` (the first
	 *  stroke's single-stroke winner, if any), then dispatch the current
	 *  event again as a fresh stroke. */
	| { type: 'fallthrough'; first: string; winner: T | undefined }
	/** Escape while pending: nothing fires. */
	| { type: 'cancelled'; first: string }
	/** Delivered through `onTimeout`: fire `winner`. */
	| { type: 'timeout'; first: string; winner: T | undefined };

export interface ChordMachineOptions<T> {
	/** Effective bindings for the current platform (read on every press, so a
	 *  re-merge takes effect without rebuilding the machine). */
	getBindings: () => readonly ChordBinding[];
	mac: boolean | (() => boolean);
	onTimeout: (outcome: Extract<ChordOutcome<T>, { type: 'timeout' }>) => void;
	timeoutMs?: number;
	setTimer?: (fn: () => void, ms: number) => unknown;
	clearTimer?: (handle: unknown) => void;
}

interface Pending<T> {
	first: string;
	winner: T | undefined;
	timer: unknown;
}

export class ChordMachine<T = unknown> {
	private pendingState: Pending<T> | null = null;
	private readonly timeoutMs: number;
	private readonly setTimer: (fn: () => void, ms: number) => unknown;
	private readonly clearTimer: (handle: unknown) => void;

	constructor(private readonly opts: ChordMachineOptions<T>) {
		this.timeoutMs = opts.timeoutMs ?? CHORD_TIMEOUT_MS;
		this.setTimer = opts.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
		this.clearTimer = opts.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
	}

	private get mac(): boolean {
		return typeof this.opts.mac === 'function' ? this.opts.mac() : this.opts.mac;
	}

	/** True while waiting for a second stroke. */
	get isPending(): boolean {
		return this.pendingState !== null;
	}

	/** The pending first stroke, if any (for a status-bar hint). */
	get pendingPrefix(): string | null {
		return this.pendingState?.first ?? null;
	}

	/**
	 * Feed one key event, as its candidate resolved strokes
	 * (`strokesFromEvent(e)` — pass `[]` never; an unmatchable event should
	 * not reach the machine). `winner` is the first stroke's single-stroke
	 * winner resolved *now* (§2.3); it is kept and handed back on timeout or
	 * fallthrough.
	 */
	press(
		strokes: readonly string[],
		ctx: WhenContext,
		winner?: T,
		evalOpts?: EvalOptions
	): ChordOutcome<T> {
		const pending = this.pendingState;
		// A bare modifier keydown (e.g. releasing ⌘ after ⌘K) resolves to no
		// strokes at all (`strokesFromEvent`). It must not cancel a pending
		// chord — leave the timer running and report `pending` unchanged.
		if (strokes.length === 0) return pending ? { type: 'pending', first: pending.first } : { type: 'none' };
		if (pending) {
			this.clearTimer(pending.timer);
			this.pendingState = null;
			if (strokes.includes('escape')) return { type: 'cancelled', first: pending.first };
			const mac = this.mac;
			for (const second of strokes) {
				const candidates = this.opts.getBindings().filter((b) => {
					const s = resolvedStrokes(b, mac);
					return (
						s.length === 2 &&
						s[0] === pending.first &&
						s[1] === second &&
						evaluateWhen(b.when, ctx, evalOpts)
					);
				});
				if (candidates.length > 0) return { type: 'chord', first: pending.first, second, candidates };
			}
			return { type: 'fallthrough', first: pending.first, winner: pending.winner };
		}

		const first = chordPrefixFor(strokes, this.opts.getBindings(), { mac: this.mac, ctx, evalOpts });
		if (first === null) return { type: 'none' };
		const state: Pending<T> = { first, winner, timer: undefined };
		state.timer = this.setTimer(() => {
			if (this.pendingState !== state) return;
			this.pendingState = null;
			this.opts.onTimeout({ type: 'timeout', first, winner });
		}, this.timeoutMs);
		this.pendingState = state;
		return { type: 'pending', first };
	}

	/** Leave chord mode without firing (window blur, unmount). */
	cancel(): void {
		if (!this.pendingState) return;
		this.clearTimer(this.pendingState.timer);
		this.pendingState = null;
	}
}
