// Pane `⋯` rows that hand out a viewer URL. In a browser session the in-app
// preview is served by the daemon's token-scoped `/__viewer/<token>/` route,
// but a copied / opened URL would outlive its pane (founder decision, gap audit
// rank 8: in-app previews only), so these rows stay hidden in remote sessions.

import { isRemoteWebSession } from '@/lib/tauri-cmd';
import type { ResolvedMenuRow } from '@/shell/menu/resolve';

export const REMOTE_HIDDEN_PANE_ROWS: ReadonlySet<string> = new Set([
	'viewer.open-in-browser',
	'viewer.copy-url',
]);

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
