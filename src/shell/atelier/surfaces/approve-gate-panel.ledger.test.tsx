/* A draft the sender's approvals ledger already approved (a Mattermost thumbs-up, say) reads
 * "Approved by @x, sending" in the Outbox and cannot be approved again, until the row's status
 * catches up. Display only: nothing here approves or sends. */

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { PausedDraftView } from '@/lib/queries/pa-actions';
import { ApproveGatePanel, approveBlockReason } from './approve-gate-panel';
import { APPROVE_GATE_FIXTURES } from './approve-gate-panel.fixtures';

afterEach(cleanup);

const SEEN = {
	channel: 'smtp',
	payloadJson: '{"item":{"subject":"s"}}',
	editedJson: null,
	scheduledAt: null,
};
const LEDGER = {
	approver: { principalId: 'p-onye', username: 'onyedikachi' },
	at: '2026-10-24T10:00:00Z',
};
const [FIRST, SECOND] = APPROVE_GATE_FIXTURES.map((d) => ({ ...d, seen: SEEN }));
const APPROVED: PausedDraftView = { ...FIRST, ledgerApproval: LEDGER };

describe('approveBlockReason with a ledger approval', () => {
	it('says who approved and that it is sending', () => {
		expect(approveBlockReason(APPROVED, false)).toBe('Approved by @onyedikachi, sending');
		expect(approveBlockReason(FIRST, false)).toBeNull();
	});

	it('leaves the stronger worker states as they were', () => {
		const sent = {
			...APPROVED,
			delivery: {
				dbStatus: 'sent',
				claimedAt: null,
				committedAt: null,
				sentAt: null,
				lastAttemptAt: null,
				externalId: null,
				deliveryStatus: null,
				deliveryCheckedAt: null,
				attempts: 1,
				scheduledAt: null,
			},
		};
		expect(approveBlockReason(sent, false)).toBe('Already sent.');
	});
});

describe('ApproveGatePanel: a ledger-approved draft', () => {
	it('shows "Approved by @x, sending" on the row and the detail, and no way to approve', () => {
		const onApprove = vi.fn();
		render(
			<ApproveGatePanel
				drafts={[APPROVED]}
				onApprove={onApprove}
				onReject={vi.fn()}
				shareNote="n"
			/>
		);
		// Row chip and the detail note.
		expect(screen.getAllByText('Approved by @onyedikachi, sending').length).toBeGreaterThanOrEqual(
			2
		);
		const btn = screen.getByRole('button', { name: /Approve & Send/ }) as HTMLButtonElement;
		expect(btn.disabled).toBe(true);
		expect(btn.title).toBe('Approved by @onyedikachi, sending');
		fireEvent.click(btn);
		expect(onApprove).not.toHaveBeenCalled();
	});

	it('an unapproved draft in the same list is unchanged', () => {
		render(
			<ApproveGatePanel
				drafts={[FIRST, { ...SECOND, ledgerApproval: LEDGER }]}
				onApprove={vi.fn()}
				onReject={vi.fn()}
				shareNote="n"
			/>
		);
		// FIRST is selected by default and is approvable.
		expect(
			(screen.getByRole('button', { name: /Approve & Send/ }) as HTMLButtonElement).disabled
		).toBe(false);
		expect(screen.queryAllByText(/Approved by @/)).toHaveLength(1);
	});

	it('is left out of Approve all', () => {
		const onApprove = vi.fn();
		render(
			<ApproveGatePanel
				drafts={[FIRST, { ...SECOND, ledgerApproval: LEDGER }]}
				onApprove={onApprove}
				onReject={vi.fn()}
			/>
		);
		fireEvent.click(screen.getByRole('checkbox', { name: `Select ${FIRST.recipient} draft` }));
		fireEvent.click(screen.getByRole('checkbox', { name: `Select ${SECOND.recipient} draft` }));
		fireEvent.click(screen.getByRole('button', { name: 'Approve all' }));
		expect(onApprove).toHaveBeenCalledTimes(1);
		expect(onApprove).toHaveBeenCalledWith(FIRST.id);
	});
});
