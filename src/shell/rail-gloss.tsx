// RailGloss — the rail's first-contact gloss (spec §1.3, v4 frame §14).
//
// The rail's steady state is the 400 ms hover tooltip on every key. This is
// the one-time introduction to the two lore nouns on the rail: a small
// callout beside the key naming it in English ("Ngwa — your equipment").
// Rules, all tested in `rail-gloss.test.tsx`:
//
//   - Shows once per profile per term. Seen terms are a JSON array under
//     `localStorage['ikenga.gloss.seen']` (shared with the v4 mockup).
//   - One at a time: the first unseen term of `terms`, in order.
//   - Never sticks: dismissed by any pointer or key activity (armed after a
//     short grace so the boot click that focused the window doesn't eat it),
//     and unconditionally after 4 s. Dismissal marks the term seen.
//   - If storage can't be read (private window, blocked site data) the gloss
//     does not show at all — without a way to remember it, "once" would
//     become "every launch".
//   - `pointer-events: none`: it never covers or captures a rail control.

import { useEffect, useState } from 'react';

export const GLOSS_SEEN_KEY = 'ikenga.gloss.seen';
/** How long the gloss stays up with no activity. */
export const GLOSS_TIMEOUT_MS = 4_000;
/** Grace before pointer / key activity dismisses it. */
export const GLOSS_ARM_MS = 600;

const DISMISS_EVENTS = ['pointerdown', 'pointermove', 'keydown'] as const;

export interface RailGlossTerm {
	/** Lore term id, e.g. `ngwa`. What gets recorded as seen. */
	term: string;
	/** One-line gloss, e.g. `Ngwa — your equipment`. */
	text: string;
	/** Key hint shown after the text (from `labelFor`), or '' for none. */
	keyLabel: string;
	/** `data-rail-item` id of the key the gloss points at. */
	anchor: string;
}

/** Seen terms, `[]` for a missing or unparsable value, or null when storage
 *  itself is unavailable. */
function readSeen(): string[] | null {
	let raw: string | null;
	try {
		raw = localStorage.getItem(GLOSS_SEEN_KEY);
	} catch {
		return null;
	}
	if (!raw) return [];
	try {
		const parsed: unknown = JSON.parse(raw);
		return Array.isArray(parsed) ? parsed.filter((t): t is string => typeof t === 'string') : [];
	} catch {
		return [];
	}
}

function markSeen(term: string): void {
	try {
		const seen = readSeen() ?? [];
		if (!seen.includes(term)) seen.push(term);
		localStorage.setItem(GLOSS_SEEN_KEY, JSON.stringify(seen));
	} catch {
		// Storage went away between read and write — nothing to remember with.
	}
}

/** The term this boot should gloss, or null. Exported for tests. */
export function pickGlossTerm(terms: readonly RailGlossTerm[]): RailGlossTerm | null {
	const seen = readSeen();
	if (seen === null) return null;
	return terms.find((t) => !seen.includes(t.term)) ?? null;
}

interface RailGlossProps {
	terms: readonly RailGlossTerm[];
	/** The rail element the gloss is positioned inside (`position: relative`). */
	railRef: React.RefObject<HTMLElement | null>;
}

export function RailGloss({ terms, railRef }: RailGlossProps) {
	// Decided once per mount: a term marked seen mid-session must not make
	// the next term pop up in its place.
	const [current, setCurrent] = useState<RailGlossTerm | null>(() => pickGlossTerm(terms));
	const [top, setTop] = useState<number | null>(null);

	// A passive effect, not a layout one: the rail's own ref is attached in
	// the same commit *after* this child's layout effects run, so it would
	// still be null there on mount. Hidden until positioned (below).
	useEffect(() => {
		if (!current) return;
		const rail = railRef.current;
		const anchor = rail?.querySelector<HTMLElement>(`[data-rail-item="${current.anchor}"]`);
		if (!rail || !anchor) return;
		const r = anchor.getBoundingClientRect();
		const rr = rail.getBoundingClientRect();
		setTop(r.top - rr.top + 8);
		anchor.setAttribute('aria-describedby', 'rail-gloss');
		return () => anchor.removeAttribute('aria-describedby');
	}, [current, railRef]);

	useEffect(() => {
		if (!current) return;
		let done = false;
		const dismiss = () => {
			if (done) return;
			done = true;
			markSeen(current.term);
			setCurrent(null);
		};
		const arm = window.setTimeout(() => {
			for (const ev of DISMISS_EVENTS) document.addEventListener(ev, dismiss, true);
		}, GLOSS_ARM_MS);
		const expire = window.setTimeout(dismiss, GLOSS_TIMEOUT_MS);
		return () => {
			window.clearTimeout(arm);
			window.clearTimeout(expire);
			for (const ev of DISMISS_EVENTS) document.removeEventListener(ev, dismiss, true);
		};
	}, [current]);

	if (!current) return null;
	return (
		<div
			id="rail-gloss"
			role="tooltip"
			data-gloss-term={current.term}
			className="ikenga-rail-gloss"
			style={top === null ? { visibility: 'hidden' } : { top }}
		>
			{current.text}
			{current.keyLabel && <kbd className="ikenga-rail-gloss-key">{current.keyLabel}</kbd>}
		</div>
	);
}
