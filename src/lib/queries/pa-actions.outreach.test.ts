import { describe, expect, it } from 'vitest';
import type { PaActionDraftRow } from '@/lib/tauri-cmd';
import { OUTREACH_SECTION, type PausedDraftView, pausedDraftFromRow } from './pa-actions';

function row(over: Partial<PaActionDraftRow> = {}, outreach?: unknown): PaActionDraftRow {
	return {
		id: 'outreach:00aa',
		batchId: 'outreach:2026-10-25',
		actionId: 'ruby/outreach-email',
		status: 'awaiting',
		channel: 'smtp',
		payloadJson: JSON.stringify({
			item: {
				id: 'x',
				channel: 'smtp',
				recipient: 'Pat',
				recipientEmail: 'pat@pub.example',
				subject: 'Hi',
				body: 'Hello',
				scheduledIso: null,
				scheduledLabel: 'unscheduled',
				...(outreach === undefined ? {} : { outreach }),
			},
			meta: { agent: 'Ruby', senderAddress: 'outreach:ada', cold: true },
		}),
		editedJson: null,
		scheduledAt: null,
		createdAt: '2026-10-25T09:00:00Z',
		committedAt: null,
		sentAt: null,
		claimedAt: null,
		attempts: 0,
		lastAttemptAt: null,
		errorText: null,
		externalId: null,
		deliveryStatus: null,
		deliveryCheckedAt: null,
		...over,
	};
}

const BLOCK = {
	assignee: 'ada',
	from_identity: 'adaugo',
	send_rank: 7,
	template: 'A',
	region: 'uk-eu',
	company: 'Pub',
	step: 'first',
};

describe('pausedDraftFromRow for an outreach draft', () => {
	it('carries the hash-bound block through fromDraftItem, which would drop it', () => {
		const d = pausedDraftFromRow(row({}, BLOCK)) as PausedDraftView;
		expect(d.outreach).toEqual(BLOCK);
		expect(d.section).toBe(OUTREACH_SECTION);
	});

	it('carries the box-reported state and pause, and nothing for a plain draft', () => {
		const d = pausedDraftFromRow(
			row(
				{
					outreachStatus: { state: 'queued_for_tomorrow', reason: 'queued_for_tomorrow' },
					outreachPause: { sentCount: 30, threshold: 30 },
				},
				BLOCK
			)
		) as PausedDraftView;
		expect(d.outreachStatus).toEqual({
			state: 'queued_for_tomorrow',
			reason: 'queued_for_tomorrow',
		});
		expect(d.outreachPause).toEqual({ sentCount: 30, threshold: 30 });

		const plain = pausedDraftFromRow(
			row({ outreachStatus: { state: 'paused' }, outreachPause: {} })
		) as PausedDraftView;
		expect(plain.outreach).toBeUndefined();
		expect('outreachStatus' in plain).toBe(false);
		expect('outreachPause' in plain).toBe(false);
		expect(plain.section).not.toBe(OUTREACH_SECTION);
	});

	it('fails soft: no status from the box is no label, and a junk one is ignored', () => {
		const none = pausedDraftFromRow(row({}, BLOCK)) as PausedDraftView;
		expect('outreachStatus' in none).toBe(false);
		const junk = pausedDraftFromRow(
			row({ outreachStatus: { state: '' }, outreachPause: null }, BLOCK)
		) as PausedDraftView;
		expect('outreachStatus' in junk).toBe(false);
		expect('outreachPause' in junk).toBe(false);
	});

	it('keeps the block through an operator edit, which can only change subject and body', () => {
		const d = pausedDraftFromRow(
			row({ editedJson: JSON.stringify({ subject: 'New', body: 'Edited' }) }, BLOCK)
		) as PausedDraftView;
		expect(d.subject).toBe('New');
		expect(d.outreach).toEqual(BLOCK);
	});

	it('does not change the row a share approval binds to', () => {
		const a = pausedDraftFromRow(row({}, BLOCK)) as PausedDraftView;
		const b = pausedDraftFromRow(
			row({ outreachStatus: { state: 'paused' }, outreachPause: {} }, BLOCK)
		) as PausedDraftView;
		expect(b.seen).toEqual(a.seen);
	});
});
