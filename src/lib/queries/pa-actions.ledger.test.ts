import { describe, expect, it } from 'vitest';
import type { PaActionDraftRow } from '@/lib/tauri-cmd';
import { ledgerApprovalOf, type PausedDraftView, pausedDraftFromRow } from './pa-actions';

const LEDGER = {
	approver: { principalId: 'p-onye', username: 'onyedikachi' },
	at: '2026-10-24T10:00:00Z',
};

function row(over: Partial<PaActionDraftRow> = {}): PaActionDraftRow {
	return {
		id: 'ruby-reply:em-1',
		batchId: 'b',
		actionId: 'a',
		status: 'awaiting',
		channel: 'smtp',
		payloadJson: JSON.stringify({
			item: {
				id: 'x',
				channel: 'smtp',
				recipient: 'Ada',
				recipientEmail: 'ada@example.com',
				subject: 'Hi',
				body: 'Hello',
			},
			meta: { agent: 'Ruby', senderAddress: 'ruby@royalti.io', cold: false },
		}),
		editedJson: null,
		scheduledAt: null,
		createdAt: '2026-10-24T09:00:00Z',
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

describe('ledgerApprovalOf', () => {
	it('is carried for a waiting row', () => {
		expect(ledgerApprovalOf(row({ ledgerApproval: LEDGER }))).toEqual(LEDGER);
		expect(ledgerApprovalOf(row({ status: 'edited', ledgerApproval: LEDGER }))).toEqual(LEDGER);
	});

	it('is ignored on any row that is not waiting, or has no readable approver', () => {
		for (const status of ['committed', 'sending', 'sent', 'failed', 'rejected']) {
			expect(ledgerApprovalOf(row({ status, ledgerApproval: LEDGER }))).toBeUndefined();
		}
		expect(ledgerApprovalOf(row())).toBeUndefined();
		expect(ledgerApprovalOf(row({ ledgerApproval: null }))).toBeUndefined();
		expect(
			ledgerApprovalOf(row({ ledgerApproval: { approver: { username: '' }, at: 't' } as never }))
		).toBeUndefined();
		expect(ledgerApprovalOf(row({ ledgerApproval: { at: 't' } as never }))).toBeUndefined();
	});
});

describe('pausedDraftFromRow with a ledger approval', () => {
	it('puts it on the view-model, and only then', () => {
		const withIt = pausedDraftFromRow(row({ ledgerApproval: LEDGER })) as PausedDraftView;
		expect(withIt.ledgerApproval).toEqual(LEDGER);
		const without = pausedDraftFromRow(row()) as PausedDraftView;
		expect('ledgerApproval' in without).toBe(false);
	});

	it('does not change the row the share approval binds to', () => {
		const a = pausedDraftFromRow(row()) as PausedDraftView;
		const b = pausedDraftFromRow(row({ ledgerApproval: LEDGER })) as PausedDraftView;
		expect(b.seen).toEqual(a.seen);
		expect(b.id).toBe(a.id);
	});
});
