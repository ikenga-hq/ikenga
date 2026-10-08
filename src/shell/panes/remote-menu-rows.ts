// Pane `⋯` rows that need the desktop's local viewer server. A browser session
// has no such server (its `http://localhost:<port>` would be the browser's own
// machine), so these rows are dropped instead of offered and failing — gap
// audit rank 8. Remove once the daemon serves an authed viewer route.

import { isRemoteWebSession } from '@/lib/tauri-cmd';
import type { ResolvedMenuRow } from '@/shell/menu/resolve';

// Formerly hid viewer.open-in-browser / viewer.copy-url (gap audit rank 8).
// Restored now that the daemon serves an authenticated viewer route.
export const REMOTE_HIDDEN_PANE_ROWS: ReadonlySet<string> = new Set([]);

/** In a browser session: drop `REMOTE_HIDDEN_PANE_ROWS` and any separator they
 *  leave leading, trailing or doubled. Desktop: the rows, untouched. */
export function dropRemoteHiddenRows(rows: ResolvedMenuRow[]): ResolvedMenuRow[] {
	if (!isRemoteWebSession()) return rows;
	const out: ResolvedMenuRow[] = [];
	for (const row of rows) {
		if (row.kind === 'item' && REMOTE_HIDDEN_PANE_ROWS.has(row.id)) continue;
		if (
			row.kind === 'separator' &&
			(out.length === 0 || out[out.length - 1].kind === 'separator')
		) {
			continue;
		}
		out.push(row);
	}
	while (out.length > 0 && out[out.length - 1].kind === 'separator') out.pop();
	return out;
}
