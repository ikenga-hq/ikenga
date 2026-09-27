// WP-69 — pure tab-label helpers for a multi-surface detached window. Kept
// out of the lazy `surfaces/surface-tab-label.tsx` so the thin root can show
// a fallback without parsing the terminal-title poll.

/** The id after the first `:` (`terminal:<ptyId>` → `<ptyId>`). */
export function surfaceSuffix(surfaceId: string): string {
	const colon = surfaceId.indexOf(':');
	return colon > 0 ? surfaceId.slice(colon + 1) : surfaceId;
}

/** The text a tab shows before (or without) its live title. */
export function fallbackTabLabel(surfaceId: string): string {
	const rest = surfaceSuffix(surfaceId);
	if (surfaceId.startsWith('terminal:')) return `terminal ${rest.slice(0, 8)}`;
	const base = rest.split('/').filter(Boolean).at(-1);
	return base ?? rest;
}
