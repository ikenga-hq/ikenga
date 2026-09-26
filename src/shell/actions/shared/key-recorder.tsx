// D-06 shared part (WP-57): the key-press recorder every keybinding-editing
// surface reuses — the Editor's key field, the Keys tab's inline "Edit" and
// its "search by pressing keys" mode. Captures one stroke (G-ACTIONS §3.1),
// converts it to the canonical `mod`-based storage grammar and hands the
// result to the caller; the caller decides what happens next (a rebind, a
// conflict check via `keyHolder()`, a search filter — this component knows
// none of that). Chord (two-stroke) recording is a follow-up: this captures
// a single stroke, which is what the Editor's key field and the Keys tab's
// row-level "Edit" both need.
//
// `src/lib/keymap/*` is WP-54's file (wave 12d, parallel to this WP) — this
// only imports its already-shipped `platform.ts` helpers, never edits them.

import { useEffect, useState } from 'react';
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
	/** Called with a canonical combo once a stroke is captured. */
	onRecord: (combo: string) => void;
	/** Recording was cancelled (Escape, blur) without a new value. */
	onCancel?: () => void;
	disabled?: boolean;
	size?: 'sm' | 'md';
	'aria-label'?: string;
	placeholder?: string;
	className?: string;
}

/** A `strokesFromEvent` result (`meta+ctrl+alt+shift+key`, no `mod`) to the
 *  canonical storage form: the platform's own primary modifier becomes
 *  `mod`, the other one stays literal (G-ACTIONS §3.1 — `ctrl` is always the
 *  literal Control key, `mod` is never combined with `ctrl` or `meta`). */
function toCanonical(resolvedStroke: string, mac: boolean): string {
	const parts = resolvedStroke.split('+');
	const key = parts[parts.length - 1];
	const mods = new Set(parts.slice(0, -1));
	const out: string[] = [];
	if (mac) {
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

/** A single button that, on click, starts listening for the next keystroke
 *  and reports it as a canonical combo. Escape cancels without recording. */
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
	const mac = isMacPlatform();
	const ariaLabel = rest['aria-label'] ?? (recording ? 'Recording — press a key combination' : 'Record a key');

	useEffect(() => {
		if (!recording) return;
		function onKeyDown(e: KeyboardEvent) {
			if (e.key === 'Escape') {
				e.preventDefault();
				e.stopPropagation();
				setRecording(false);
				onCancel?.();
				return;
			}
			const strokes = strokesFromEvent(e);
			if (strokes.length === 0) return; // modifier-only, IME, Dead key — keep waiting
			e.preventDefault();
			e.stopPropagation();
			const combo = toCanonical(strokes[0], mac);
			if (validateKeySequence(combo) !== null) return; // shouldn't happen; stay recording
			setRecording(false);
			onRecord(combo);
		}
		window.addEventListener('keydown', onKeyDown, true);
		return () => window.removeEventListener('keydown', onKeyDown, true);
	}, [recording, mac, onRecord, onCancel]);

	function onBlur() {
		if (recording) {
			setRecording(false);
			onCancel?.();
		}
	}

	return (
		<button
			type="button"
			disabled={disabled}
			aria-label={ariaLabel}
			data-recording={recording || undefined}
			onClick={() => setRecording((r) => !r)}
			onBlur={onBlur}
			className={cn('rec', size === 'sm' && 'sm', className)}
		>
			{recording ? (
				<span className="lbl recording">Press a key…</span>
			) : value ? (
				<Kbd combo={value} mac={mac} />
			) : (
				<span className="lbl empty">{placeholder}</span>
			)}
		</button>
	);
}
