// The remote inbox's read model (G-ACCESS §5.7, WP-75). The daemon's
// post-hook annotates every `permission` row with `can_decide` /
// `waiting_on` for THIS request (and `can_allow_always`); the card renders
// read-only with the reason whenever `can_decide` is false (D-05
// `remote-client` side note, D-7). The daemon decides — this only words it.

import { TIER_LABELS } from '@/lib/access/caps.gen';
import type { AccessStatus } from '@/lib/access/client';

import { type AnnotatedRow, inboxReadOnlyReason } from './remote-model';

/** The post-hook's fields (WP-75 adds `can_allow_always`). */
export type InboxRow = AnnotatedRow & { can_allow_always?: boolean };

/**
 * Why a card is read-only on this device, or `null` when it is live.
 * `routedDevice` is the device "this device only" names, when known.
 */
export function inboxCardReason(
	row: InboxRow,
	status: AccessStatus,
	routedDevice: string | null
): string | null {
	if (row.resolvedAt) return 'Already answered';
	if (row.can_decide === true) return null;
	switch (row.waiting_on) {
		case 'owner':
			return 'Waiting on the Owner';
		case 'device':
			return routedDevice
				? `Answer on ${routedDevice} (this device only)`
				: 'Answer on the device chosen for asks (this device only)';
		case 'approve':
			return `This device can't approve — it is ${TIER_LABELS[status.credential.tier].label}`;
		default:
			break;
	}
	if (row.dedupeKey?.startsWith('permission:terminal:')) {
		return 'Claude is asking in its terminal — answer it there';
	}
	return inboxReadOnlyReason(row, status);
}

/** "Always for this project" only where the engine offers it and this
 *  decider may persist a rule (members never do, §5.4 3d). */
export function offersAlways(row: InboxRow): boolean {
	return row.can_decide === true && row.can_allow_always === true;
}

/** Whether any card waits on the routed device (worth naming it). */
export function needsRoutedDevice(rows: readonly InboxRow[]): boolean {
	return rows.some((r) => !r.resolvedAt && r.waiting_on === 'device');
}

/** The refusal copy for a decide that raced a change (§9.1 codes). */
export function decideErrorCopy(code: string | null, message: string): string {
	switch (code) {
		case 'conflict':
			return 'Already answered, or it timed out.';
		case 'routing_refused':
			return 'Asks are answered on another device now.';
		case 'owner_approval_required':
			return 'This one needs the Owner.';
		case 'answer_in_terminal':
			return 'Answer this in its terminal.';
		default:
			return message;
	}
}
