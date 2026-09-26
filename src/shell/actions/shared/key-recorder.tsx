// D-06 shared part (WP-57): the key-press recorder every keybinding-editing
// surface reuses — the Editor's key field, the Keys tab's inline "Edit" and
// its "search by pressing keys" mode. Captures one stroke, or a two-stroke
// chord when the second stroke arrives within the DEC-57 900ms window,
// converts it to the canonical `mod`-based storage grammar and hands the
// result to the caller; the caller decides what happens next (a rebind, a
// conflict check via `keyHolder()`, a search filter — this component knows
// none of that).
//
// `src/lib/keymap/*` is WP-54's file (wave 12d, parallel to this WP) — this
// only imports its already-shipped `platform.ts` helpers, never edits them.

import { useEffect, useRef, useState } from 'react';
import {
	canonicalizeKeySequence,
	isMacPlatform,
	strokesFromEvent,
	validateKeySequence,
} from '@/lib/keymap/platform';
import { cn } from '@/components/ui/utils';
import { Kbd } from './kbd';

export interface KeyRecorderProps {
	/** The current combo (canonical form), or null/undefined for none set. */
	value?: string | null;
	/** Called with a canonical combo once a stroke (or chord) is captured.
	 *  Called with `''` when Backspace clears the current binding. */
	onRecord: (combo: string) => void;
	/** Recording was cancelled (Escape, blur) without a new value. */
	onCancel?: () => void;
	disabled?: boolean;
	size?: 'sm' | 'md';
	'aria-label'?: string;
	placeholder?: string;
	className?: string;
}

/** DEC-57: a second stroke within this window forms a chord with the first;
 *  past it, the first stroke commits on its own. */
const CHORD_WINDOW_MS = 900;

/**
 * A `strokesFromEvent` result (`meta+ctrl+alt+shift+key`, no `mod`) to the
 * canonical storage form: the platform's own primary modifier becomes `mod`.
 * §3.1's grammar forbids combining `mod` with `ctrl` or `meta` — when BOTH
 * physical modifiers are held at once (⌃⌘ on mac, Ctrl+Win elsewhere), the
 * only valid encoding is the two literal modifiers, `ctrl+meta`, never
 * `mod+ctrl` or `mod+meta`.
 */
function toCanonical(resolvedStroke: string, mac: boolean): string {
	const parts = resolvedStroke.split('+');
	const key = parts[parts.length - 1];
	const mods = new Set(parts.slice(0, -1));
	const out: string[] = [];
	if (mods.has('meta') && mods.has('ctrl')) {
		out.push('ctrl', 'meta');
	} else if (mac) {
		if (mods.has('meta')) out.push('mod');
		if (mods.has('ctrl')) out.push('ctrl');
	} else {
		if (mods.has('ctrl')) out.push('mod');
		if (mods.has('meta')) out.push('meta');
	}
	if (mods.has('alt')) out.push('alt');
	if (mods.has('shift')) out.push('shift');
	out.push(key);
	return canonicalizeKeySequence(out.join('+'));
}

/** A single button that, on click, starts listening for a stroke (or a
 *  chord) and reports it as a canonical combo. Escape cancels without
 *  recording; Backspace on the first stroke clears instead of recording
 *  "backspace" as the new binding; an invalid sequence shows inline feedback
 *  and lets the user retry without leaving recording mode. */
export function KeyRecorder({
	value,
	onRecord,
	onCancel,
	disabled,
	size = 'md',
	placeholder = 'Press keys…',
	className,
	...rest
}: KeyRecorderProps) {
	const [recording, setRecording] = useState(false);
	const [firstStroke, setFirstStroke] = useState<string | null>(null);
	const [invalid, setInvalid] = useState(false);
	const chordTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
	const mac = isMacPlatform();
	const ariaLabel =
		rest['aria-label'] ??
		(recording
			? firstStroke
				? 'Recording — press a second key for a chord, or wait to record one key'
				: 'Recording — press a key combination'
			: 'Record a key');

	function clearChordTimer() {
		if (chordTimer.current) {
			clearTimeout(chordTimer.current);
			chordTimer.current = null;
		}
	}

	function finish(combo: string) {
		clearChordTimer();
		setRecording(false);
		setFirstStroke(null);
		setInvalid(false);
		onRecord(combo);
	}

	function cancel() {
		clearChordTimer();
		setRecording(false);
		setFirstStroke(null);
		setInvalid(false);
		onCancel?.();
	}

	function start() {
		setInvalid(false);
		setFirstStroke(null);
		setRecording(true);
	}

	useEffect(() => {
		if (!recording) return;
		function onKeyDown(e: KeyboardEvent) {
			if (e.key === 'Escape') {
				e.preventDefault();
				e.stopPropagation();
				cancel();
				return;
			}
			if (e.key === 'Backspace' && !firstStroke) {
				e.preventDefault();
				e.stopPropagation();
				finish('');
				return;
			}
			const strokes = strokesFromEvent(e);
			if (strokes.length === 0) return; // modifier-only, IME, Dead key — keep waiting
			e.preventDefault();
			e.stopPropagation();
			const stroke = toCanonical(strokes[0], mac);

			if (!firstStroke) {
				if (validateKeySequence(stroke) !== null) {
					setInvalid(true);
					return;
				}
				// A chord's second stroke may still arrive within the window
				// (DEC-57); commit as a single stroke if nothing follows in time.
				setFirstStroke(stroke);
				setInvalid(false);
				clearChordTimer();
				chordTimer.current = setTimeout(() => finish(stroke), CHORD_WINDOW_MS);
				return;
			}

			const chord = `${firstStroke} ${stroke}`;
			if (validateKeySequence(chord) !== null) {
				// Let the user retry the whole sequence rather than getting stuck
				// waiting on a second stroke that can never complete it.
				clearChordTimer();
				setFirstStroke(null);
				setInvalid(true);
				return;
			}
			finish(chord);
		}
		window.addEventListener('keydown', onKeyDown, true);
		return () => window.removeEventListener('keydown', onKeyDown, true);
	}, [recording, firstStroke, mac]);

	useEffect(() => () => clearChordTimer(), []);

	function onBlur() {
		if (recording) cancel();
	}

	return (
		<button
			type="button"
			disabled={disabled}
			aria-label={ariaLabel}
			data-recording={recording || undefined}
			data-invalid={invalid || undefined}
			onClick={() => (recording ? cancel() : start())}
			onBlur={onBlur}
			className={cn('rec', size === 'sm' && 'sm', className)}
		>
			{recording ? (
				invalid ? (
					<span className="lbl invalid" role="alert">
						Not a valid key — try again
					</span>
				) : firstStroke ? (
					<span className="lbl recording">
						<Kbd combo={firstStroke} mac={mac} /> then…
					</span>
				) : (
					<span className="lbl recording">Press a key…</span>
				)
			) : value ? (
				<Kbd combo={value} mac={mac} />
			) : (
				<span className="lbl empty">{placeholder}</span>
			)}
		</button>
	);
}
