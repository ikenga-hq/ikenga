/* Publisher outreach in the Outbox (WP-D): one ranked queue per person, the sender's labels next
 * to the existing "Approved by @x, sending", the pause banner, and no batch approval. Display
 * only: who may see or act on a row is enforced by the box's broker, not here. */

import { cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { OUTREACH_SECTION, type PausedDraftView } from '@/lib/queries/pa-actions';
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
	approver: { principalId: 'p-ada', username: 'ada' },
	at: '2026-10-26T08:00:00Z',
};
const BASE = APPROVE_GATE_FIXTURES[0];

function outreach(
	id: string,
	assignee: string,
	rank: number,
	over: Partial<PausedDraftView> = {}
): PausedDraftView {
	return {
		...BASE,
		id,
		recipient: `Pub ${id}`,
		subject: `Subject ${id}`,
		section: OUTREACH_SECTION,
		status: 'awaiting',
		everEdited: false,
		overdue: false,
		timeVariant: null,
		seen: SEEN,
		outreach: { assignee, send_rank: rank, company: `Pub ${id}` },
		...over,
	};
}

function plain(id: string): PausedDraftView {
	return { ...BASE, id, recipient: `Reply ${id}`, subject: `Re ${id}`, seen: SEEN };
}

/** The row recipients, top to bottom. */
function order(container: HTMLElement): string[] {
	return [...container.querySelectorAll('.ob-row')].map(
		(r) => r.getAttribute('aria-label')?.split(' – ')[0] ?? ''
	);
}

function mount(
	drafts: PausedDraftView[],
	props: Partial<Parameters<typeof ApproveGatePanel>[0]> = {}
) {
	return render(
		<ApproveGatePanel
			drafts={drafts}
			onApprove={vi.fn()}
			onReject={vi.fn()}
			shareNote="share"
			{...props}
		/>
	);
}

describe('the outreach queue order', () => {
	it("lists one person's queue by send_rank ascending", () => {
		const { container } = mount([
			outreach('r3', 'ada', 3),
			outreach('r1', 'ada', 1),
			outreach('r10', 'ada', 10),
			outreach('r2', 'ada', 2),
		]);
		expect(order(container)).toEqual(['Pub r1', 'Pub r2', 'Pub r3', 'Pub r10']);
	});

	it('groups the all-view by assignee and then rank, and says whose each row is', () => {
		const { container } = mount([
			outreach('c2', 'chinedum', 2),
			outreach('a2', 'ada', 2),
			outreach('c1', 'chinedum', 1),
			outreach('a1', 'ada', 1),
		]);
		expect(order(container)).toEqual(['Pub a1', 'Pub a2', 'Pub c1', 'Pub c2']);
		expect(container.querySelectorAll('.outreach-assignee')).toHaveLength(4);
		expect(screen.getAllByText('@ada')).toHaveLength(2);
	});

	it('shows the rank on each row, and the assignee only when more than one queue is on screen', () => {
		const { container } = mount([outreach('r2', 'ada', 2), outreach('r1', 'ada', 1)]);
		expect([...container.querySelectorAll('.outreach-rank')].map((e) => e.textContent)).toEqual([
			'#1',
			'#2',
		]);
		expect(container.querySelector('.outreach-assignee')).toBeNull();
	});

	it('keeps outreach in one section and leaves plain drafts where they were', () => {
		const { container } = mount([
			plain('p1'),
			outreach('r2', 'ada', 2),
			plain('p2'),
			outreach('r1', 'ada', 1),
		]);
		// Plain drafts are in their own (non-outreach) section as before; the outreach queue is one section.
		const names = order(container);
		expect(names.filter((n) => n.startsWith('Reply'))).toEqual(['Reply p1', 'Reply p2']);
		expect(names.filter((n) => n.startsWith('Pub'))).toEqual(['Pub r1', 'Pub r2']);
		expect(container.textContent).toContain(OUTREACH_SECTION);
	});

	it('leaves an all-plain queue exactly as it was', () => {
		const { container } = mount([plain('b'), plain('a'), plain('c')]);
		expect(order(container)).toEqual(['Reply b', 'Reply a', 'Reply c']);
		expect(container.querySelector('.outreach-rank')).toBeNull();
	});
});

describe('outreach labels next to the ledger approval', () => {
	it('keeps the rank order and shows "Approved by @x, sending" on an approved row', () => {
		const { container } = mount([
			outreach('r2', 'ada', 2),
			outreach('r1', 'ada', 1, { ledgerApproval: LEDGER }),
		]);
		expect(order(container)).toEqual(['Pub r1', 'Pub r2']);
		const first = container.querySelectorAll('.ob-row')[0] as HTMLElement;
		expect(within(first).getByText('Approved by @ada, sending')).toBeTruthy();
		expect(within(first).getByText('#1')).toBeTruthy();
		const second = container.querySelectorAll('.ob-row')[1] as HTMLElement;
		expect(within(second).queryByText(/Approved by/)).toBeNull();
	});

	it('says "queued for tomorrow" for an approved row the day\'s cap is holding, not "sending"', () => {
		const { container } = mount([
			outreach('r1', 'ada', 1, {
				ledgerApproval: LEDGER,
				outreachStatus: { state: 'queued_for_tomorrow' },
			}),
		]);
		const row = container.querySelector('.ob-row') as HTMLElement;
		expect(within(row).getByText('Approved by @ada, queued for tomorrow')).toBeTruthy();
		expect(within(row).queryByText(/sending/)).toBeNull();
		// Not approvable again, with the same words.
		const view = outreach('r1', 'ada', 1, {
			ledgerApproval: LEDGER,
			outreachStatus: { state: 'queued_for_tomorrow' },
		});
		expect(approveBlockReason(view, false)).toBe('Approved by @ada, queued for tomorrow');
	});

	it('labels a suppressed or paused row that has no approval with a chip of its own', () => {
		const { container } = mount([
			outreach('r1', 'ada', 1, {
				outreachStatus: { state: 'suppressed', reason: 'suppressed:to_email' },
			}),
			outreach('r2', 'ada', 2, { outreachStatus: { state: 'paused' } }),
			outreach('r3', 'ada', 3),
		]);
		const rows = container.querySelectorAll('.ob-row');
		expect(within(rows[0] as HTMLElement).getByText('suppressed')).toBeTruthy();
		expect(within(rows[1] as HTMLElement).getByText('paused')).toBeTruthy();
		expect(within(rows[2] as HTMLElement).queryByText(/suppressed|paused|queued/)).toBeNull();
	});

	it('shows a suppressed approved row as held, never as sending', () => {
		const view = outreach('r1', 'ada', 1, {
			ledgerApproval: LEDGER,
			outreachStatus: { state: 'suppressed' },
		});
		expect(approveBlockReason(view, false)).toBe(
			'Approved by @ada, on the do-not-contact list, it will not send'
		);
	});

	it('fails soft: with no status from the box the rows show, ranked and unlabelled', () => {
		const { container } = mount([outreach('r2', 'ada', 2), outreach('r1', 'ada', 1)]);
		expect(order(container)).toEqual(['Pub r1', 'Pub r2']);
		expect(container.querySelector('.dl-chip')).toBeNull();
		expect(container.querySelector('.ob-outreach-paused')).toBeNull();
	});
});

describe('the pause banner', () => {
	it('is shown while any outreach row carries the pause, with the progress', () => {
		mount([
			outreach('r1', 'ada', 1, { outreachPause: { sentCount: 30, threshold: 30 } }),
			outreach('r2', 'ada', 2, { outreachPause: { sentCount: 30, threshold: 30 } }),
		]);
		const banner = screen.getByText(/Sending is paused until Chinedum resumes/);
		expect(banner.textContent).toContain('30 of 30 sent');
		expect(banner.getAttribute('role')).toBe('status');
	});

	it('is shown for a pause with no detail, and absent without one', () => {
		const { unmount } = mount([outreach('r1', 'ada', 1, { outreachPause: {} })]);
		expect(screen.getByText('Sending is paused until Chinedum resumes.')).toBeTruthy();
		unmount();
		mount([outreach('r1', 'ada', 1)]);
		expect(screen.queryByText(/Sending is paused/)).toBeNull();
	});
});

describe('no batch approval of outreach', () => {
	it('Approve all skips outreach rows and approves the plain ones as before', () => {
		const onApprove = vi.fn();
		mount([plain('p1'), outreach('r1', 'ada', 1), outreach('r2', 'ada', 2)], {
			onApprove,
			shareNote: undefined,
		});
		for (const name of ['Reply p1', 'Pub r1', 'Pub r2']) {
			fireEvent.click(screen.getByRole('checkbox', { name: `Select ${name} draft` }));
		}
		expect(screen.getByText('Outreach emails are approved one at a time.')).toBeTruthy();
		fireEvent.click(screen.getByRole('button', { name: 'Approve all' }));
		expect(onApprove).toHaveBeenCalledTimes(1);
		expect(onApprove).toHaveBeenCalledWith('p1');
	});

	it('Approve all is off when only outreach rows are selected', () => {
		const onApprove = vi.fn();
		mount([outreach('r1', 'ada', 1), outreach('r2', 'ada', 2)], {
			onApprove,
			shareNote: undefined,
		});
		fireEvent.click(screen.getByRole('checkbox', { name: 'Select Pub r1 draft' }));
		fireEvent.click(screen.getByRole('checkbox', { name: 'Select Pub r2 draft' }));
		const btn = screen.getByRole('button', { name: 'Approve all' }) as HTMLButtonElement;
		expect(btn.disabled).toBe(true);
		fireEvent.click(btn);
		expect(onApprove).not.toHaveBeenCalled();
	});

	it('has no Approve all in a share, as before', () => {
		mount([outreach('r1', 'ada', 1)]);
		fireEvent.click(screen.getByRole('checkbox', { name: 'Select Pub r1 draft' }));
		expect(screen.queryByRole('button', { name: 'Approve all' })).toBeNull();
	});
});
