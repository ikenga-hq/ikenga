/* Publisher outreach drafts in the Outbox (box plan 2026-10-25-box-outreach-sender, WP-D).
 *
 * Each outreach draft carries a hash-bound `item.outreach` block (approvals contract r5 §3.8.1):
 * the person it belongs to and its place in that person's queue. The shell mirrors the block
 * here, as a type of its own, because `DraftItem` in `@ikenga/contract` does not know it yet and
 * `fromDraftItem` drops what it does not know.
 *
 * Everything in this file is DISPLAY. Who may see or act on a row is decided by the box's broker
 * (`broker/approvals.rs`), and what the sender actually does with a row is decided by the sender;
 * a label here never gates a send, and a row missing a label is not a row that will not send. */

import type { DraftItem } from '@ikenga/contract';

/** `item.outreach`, as the importer writes it (contract r5 §3.8.1). Optional members are
 *  tolerated missing so a newer sender cannot blank the panel. */
export interface OutreachBlock {
	assignee: string;
	from_identity?: string;
	/** 1-based place in the assignee's queue; lower sends first. */
	send_rank?: number;
	template?: string;
	region?: string;
	company?: string;
	step?: 'first' | 'followup' | string;
}

/** `DraftItem` plus the outreach member the contract package has not published yet. */
export type DraftItemWithOutreach = DraftItem & { outreach?: OutreachBlock };

/** The sender's state for one outreach row (contract r5 §4.6), added to the listed row by the
 *  box. Display only. */
export type OutreachState = 'queued_for_tomorrow' | 'paused' | 'suppressed' | 'blocked' | 'sent';

export interface OutreachRowStatus {
	state: OutreachState | string;
	reason?: string;
	updatedAt?: string;
}

/** The open global pause (30 sends), added to every outreach row while it lasts. */
export interface OutreachPause {
	at?: string;
	sentCount?: number;
	threshold?: number;
}

/** The outreach block of a parsed `item`, or undefined when there is no usable one. A block
 *  without a string assignee is not shown as outreach: the box decides visibility, this only
 *  decides whether to draw rank and labels. */
export function outreachOf(item: unknown): OutreachBlock | undefined {
	if (item === null || typeof item !== 'object') return undefined;
	const block = (item as { outreach?: unknown }).outreach;
	if (block === null || typeof block !== 'object') return undefined;
	const b = block as Record<string, unknown>;
	if (typeof b.assignee !== 'string' || b.assignee === '') return undefined;
	const out: OutreachBlock = { assignee: b.assignee };
	if (typeof b.from_identity === 'string') out.from_identity = b.from_identity;
	if (typeof b.send_rank === 'number' && Number.isFinite(b.send_rank)) out.send_rank = b.send_rank;
	if (typeof b.template === 'string') out.template = b.template;
	if (typeof b.region === 'string') out.region = b.region;
	if (typeof b.company === 'string') out.company = b.company;
	if (typeof b.step === 'string') out.step = b.step;
	return out;
}

/** Anything the panel can order: a draft that may carry an outreach block. */
export interface MaybeOutreach {
	outreach?: OutreachBlock;
}

/** Order outreach drafts for display: by assignee, then `send_rank` ascending (a row with no rank
 *  last), ties kept in the order given. Rows that are not outreach keep their exact places:
 *  the slots the outreach rows occupied are refilled in the new order. */
export function orderOutreach<T extends MaybeOutreach>(rows: readonly T[]): T[] {
	const slots: number[] = [];
	const outreach: T[] = [];
	rows.forEach((r, i) => {
		if (r.outreach) {
			slots.push(i);
			outreach.push(r);
		}
	});
	if (outreach.length < 2) return [...rows];
	const keyed = outreach.map((r, i) => ({ r, i }));
	keyed.sort((a, b) => {
		const byWho = (a.r.outreach?.assignee ?? '').localeCompare(b.r.outreach?.assignee ?? '');
		if (byWho !== 0) return byWho;
		const ra = a.r.outreach?.send_rank ?? Number.POSITIVE_INFINITY;
		const rb = b.r.outreach?.send_rank ?? Number.POSITIVE_INFINITY;
		if (ra !== rb) return ra < rb ? -1 : 1;
		return a.i - b.i;
	});
	const out = [...rows];
	slots.forEach((slot, k) => {
		out[slot] = keyed[k].r;
	});
	return out;
}

/** The label for a row's sender state, or null when there is nothing to say. */
export function outreachStateLabel(status: OutreachRowStatus | undefined): string | null {
	switch (status?.state) {
		case 'queued_for_tomorrow':
			return 'queued for tomorrow';
		case 'suppressed':
			return 'suppressed';
		case 'paused':
			return 'paused';
		case 'blocked':
			return 'blocked';
		default:
			return null;
	}
}

/** Why the sender will not send an approved row right now, in words; null when it is on its way. */
export function outreachHoldNote(status: OutreachRowStatus | undefined): string | null {
	switch (status?.state) {
		case 'queued_for_tomorrow':
			return 'queued for tomorrow';
		case 'paused':
			return 'waiting: sending is paused';
		case 'suppressed':
			return 'on the do-not-contact list, it will not send';
		case 'blocked':
			return 'blocked, it will not send';
		default:
			return null;
	}
}

/** The banner shown while the sender is paused, or null. */
export function outreachPauseText(pause: OutreachPause | undefined): string | null {
	if (!pause) return null;
	const n = pause.sentCount;
	const of = pause.threshold;
	const progress = typeof n === 'number' && typeof of === 'number' ? ` (${n} of ${of} sent)` : '';
	return `Sending is paused until Chinedum resumes${progress}.`;
}
