// Which detected engines the Companion's target picker (and its ⌥↑/⌥↓ cycle)
// may offer as runnable targets. Pure — kept apart from `target-picker.tsx`
// so it tests without the UI tree.

import type { DetectedAgent } from '@/lib/tauri-cmd';

/** Engines a *New session on…* / *Persistent run* row may name: the default
 *  first, then every detected engine that isn't known to be signed out.
 *
 *  An engine detection couldn't check (WSL couldn't be asked, D-10) is
 *  handled asymmetrically:
 *  - the default engine stays offered (D-11: a WSL-only engine stays
 *    seatable; the run itself fails with the clear WSL reason) — the row is
 *    tagged via `engineOfferNote`;
 *  - any other unchecked engine is not offered. When WSL is down every known
 *    engine missing from the host comes back unchecked, most of which the
 *    user never installed, and before D-10 none of them were offered. */
export function offeredEngines(
	defaultEngineId: string | null,
	detected: readonly DetectedAgent[] | undefined
): string[] {
	const out: string[] = [];
	if (defaultEngineId && detected?.find((a) => a.id === defaultEngineId)?.authed !== false)
		out.push(defaultEngineId);
	for (const a of detected ?? []) {
		if (a.authed === false || a.unavailable || out.includes(a.id)) continue;
		out.push(a.id);
	}
	return out;
}

/** Trailing note for an offered engine's picker row: `WSL unavailable` when
 *  detection couldn't check it, else null. */
export function engineOfferNote(
	engineId: string,
	detected: readonly DetectedAgent[] | undefined
): string | null {
	return detected?.find((a) => a.id === engineId)?.unavailable ? 'WSL unavailable' : null;
}

/** The engine to name in the picker's empty state when detection couldn't
 *  check it: the default engine if it is the unavailable one, else the first
 *  unavailable engine. Null when every engine was checked. */
export function unavailableEngine(
	defaultEngineId: string | null,
	detected: readonly DetectedAgent[] | undefined
): DetectedAgent | null {
	const all = (detected ?? []).filter((a) => a.unavailable);
	return all.find((a) => a.id === defaultEngineId) ?? all[0] ?? null;
}
