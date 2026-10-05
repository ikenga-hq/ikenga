// Pure helpers for the remote client (G-ACCESS §3.12, §5.7; D-05
// `remote-client`). WP-74b.

import { TIER_LABELS } from '@/lib/access/caps.gen';
import type { AccessStatus } from '@/lib/access/client';
import type { ChiCacheRow, NotificationRow, TerminalDescriptor } from '@/lib/tauri-cmd';

/** §5.7: the post-hook annotates permission rows (WP-75). Absent until then. */
export type AnnotatedRow = NotificationRow & {
	can_decide?: boolean;
	waiting_on?: null | 'owner' | 'device' | 'approve';
};

/**
 * Why a permission card is read-only on this device, or `null` when it is
 * live. A card is live only when its row says `can_decide` (§3.12, §5.7;
 * D-7: a `dispatch` device sees the inbox read-only with its reason).
 */
export function inboxReadOnlyReason(row: AnnotatedRow, status: AccessStatus): string | null {
	if (row.resolvedAt) return 'Already answered';
	const tier = TIER_LABELS[status.credential.tier].label;
	if (row.can_decide === true) return null;
	switch (row.waiting_on) {
		case 'owner':
			return 'Waiting on the Owner';
		case 'device':
			return 'Answer on the device chosen for asks (this device only)';
		case 'approve':
			return `This device can't approve — it is ${tier}`;
		default:
			break;
	}
	if (!status.caps.includes('approve')) return `This device can't approve — it is ${tier}`;
	return 'Answer this on the computer for now';
}

export interface SessionRow {
	id: string;
	label: string;
	detail: string;
	tone: 'live' | 'muted' | 'warn';
	/** A terminal this device may type into (the dispatch bar's targets). */
	ptyId: string | null;
}

/** Live terminals first, then recent Chi runs. */
export function sessionRows(terms: TerminalDescriptor[], runs: ChiCacheRow[]): SessionRow[] {
	const rows: SessionRow[] = terms
		.filter((t) => t.status === 'running')
		.map((t) => ({
			id: `pty:${t.pty_id}`,
			label: t.label || t.title || t.argv.join(' ') || 'terminal',
			detail: t.foreground_command?.name ? `live · ${t.foreground_command.name}` : 'live',
			tone: 'live' as const,
			ptyId: t.pty_id,
		}));
	for (const r of runs.slice(0, 8)) {
		const running = r.status === 'running' || r.status === 'starting';
		rows.push({
			id: `run:${r.run_id}`,
			label: `${r.engine_id} · ${r.brief?.slice(0, 40) || r.run_id.slice(0, 8)}`,
			detail: r.status,
			tone: running ? 'warn' : 'muted',
			ptyId: null,
		});
	}
	return rows;
}

/** "Pixel 9 · Chrome · View + dispatch". */
export function credentialLine(status: AccessStatus): string {
	return TIER_LABELS[status.credential.tier].label;
}
