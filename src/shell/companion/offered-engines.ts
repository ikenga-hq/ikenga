// Which detected engines the Companion's target picker (and its ⌥↑/⌥↓ cycle)
// may offer as runnable targets. Pure — kept apart from `target-picker.tsx`
// so it tests without the UI tree.

import type { DetectedAgent } from '@/lib/tauri-cmd';

/** Engines a *New session on…* / *Persistent run* row may name: the default
 *  first, then every detected engine that isn't known to be signed out. An
 *  engine detection couldn't check (WSL couldn't be asked, D-10) is not
 *  runnable, so it is never offered. */
export function offeredEngines(
	defaultEngineId: string | null,
	detected: readonly DetectedAgent[] | undefined
): string[] {
	const excluded = (a: DetectedAgent | undefined) => a?.authed === false || !!a?.unavailable;
	const out: string[] = [];
	if (defaultEngineId && !excluded(detected?.find((a) => a.id === defaultEngineId)))
		out.push(defaultEngineId);
	for (const a of detected ?? []) {
		if (excluded(a) || out.includes(a.id)) continue;
		out.push(a.id);
	}
	return out;
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
