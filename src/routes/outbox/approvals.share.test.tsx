/* /outbox/approvals inside a share (box plan WP-5): the commit carries the row that
 * was shown, a refusal from the broker is displayed on the draft, a Reviewer sees why
 * there is no queue, and outside a share the commit stays {draftId} only. */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const share = vi.hoisted(() => ({ current: null as null | { role: string } }));

vi.mock('@/shell/people/use-shared-projects', () => ({
	useCurrentShare: () => share.current,
}));
vi.mock('@/lib/tauri-cmd', () => ({
	paActionsList: vi.fn(),
	paActionsCommit: vi.fn(),
	paActionsReject: vi.fn(),
	paActionsRetry: vi.fn(),
	paActionsUpdate: vi.fn(),
}));

import type { PaActionDraftRow } from '@/lib/tauri-cmd';
import { paActionsCommit, paActionsList } from '@/lib/tauri-cmd';
import { Route } from './approvals';

const ROW: PaActionDraftRow = {
	id: 'ruby-reply:em-1001',
	batchId: 'b',
	actionId: 'ruby/email-reply',
	status: 'awaiting',
	channel: 'smtp',
	payloadJson: JSON.stringify({
		item: {
			id: 'ruby-reply:em-1001',
			recipient: 'Pat Customer',
			recipientEmail: 'pat@example.com',
			subject: 'Re: Invoice 42',
			body: 'Hi Pat',
			channel: 'smtp',
			senderAddress: 'ruby@royalti.io',
			fromProvider: 'SMTP · ruby@royalti.io',
			scheduledIso: null,
			scheduledLabel: 'on approval',
		},
		meta: {
			actionId: 'ruby/email-reply',
			actionName: 'Ruby email reply',
			agent: 'Ruby',
			model: 'Sonnet',
		},
	}),
	editedJson: null,
	scheduledAt: null,
	createdAt: '2026-10-20 09:00:00',
	committedAt: null,
	sentAt: null,
	claimedAt: null,
	attempts: 0,
	lastAttemptAt: null,
	errorText: null,
	externalId: null,
	deliveryStatus: null,
	deliveryCheckedAt: null,
};

function Page() {
	const C = Route.options.component as () => React.ReactElement;
	return <C />;
}

function mount() {
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return render(
		<QueryClientProvider client={qc}>
			<Page />
		</QueryClientProvider>
	);
}

async function approve() {
	await screen.findByLabelText('Email subject');
	vi.useFakeTimers({ shouldAdvanceTime: true });
	fireEvent.click(screen.getByRole('button', { name: /Approve & Send/ }));
	for (let i = 0; i < 12; i++) {
		await act(async () => {
			await vi.advanceTimersByTimeAsync(1000);
		});
	}
	vi.useRealTimers();
}

beforeEach(() => {
	vi.mocked(paActionsList).mockResolvedValue([ROW]);
	vi.mocked(paActionsCommit).mockReset();
});
afterEach(() => {
	cleanup();
	vi.useRealTimers();
	share.current = null;
});

describe('/outbox/approvals in a share', () => {
	it('commits with the row that was shown and notes that the box sends', async () => {
		share.current = { role: 'operator' };
		vi.mocked(paActionsCommit).mockResolvedValue(undefined);
		mount();
		expect((await screen.findByRole('note')).textContent).toMatch(/box/);
		await approve();
		await waitFor(() => expect(paActionsCommit).toHaveBeenCalledTimes(1));
		expect(paActionsCommit).toHaveBeenCalledWith(ROW.id, {
			channel: 'smtp',
			payloadJson: ROW.payloadJson,
			editedJson: null,
			scheduledAt: null,
		});
	});

	it('shows the broker refusal on the draft, in plain words, and keeps the draft', async () => {
		share.current = { role: 'operator' };
		vi.mocked(paActionsCommit).mockRejectedValue(
			new Error(
				"approval_refused: Nothing was approved: you are not on the box's list of people who may approve sends."
			)
		);
		mount();
		await approve();
		const alert = await screen.findByRole('alert', { name: 'Approval error' });
		expect(alert.textContent).toContain(
			"Nothing was approved: you are not on the box's list of people who may approve sends."
		);
		// The code prefix is not shown to the person.
		expect(alert.textContent).not.toContain('approval_refused');
		expect(screen.getByLabelText('Email subject')).toBeTruthy();
	});

	it('shows "approval service unreachable" as a failed approval, not a silent success', async () => {
		share.current = { role: 'operator' };
		vi.mocked(paActionsCommit).mockRejectedValue(
			new Error(
				'approval_unavailable: Nothing was approved: the box’s approval service is not reachable.'
			)
		);
		mount();
		await approve();
		const alert = await screen.findByRole('alert', { name: 'Approval error' });
		expect(alert.textContent).toMatch(/approval service is not reachable/);
	});

	it('explains why a Reviewer has no queue instead of showing an empty one', async () => {
		share.current = { role: 'reviewer' };
		vi.mocked(paActionsList).mockRejectedValue(
			new Error(
				'forbidden: reviewing and approving drafts in a shared project needs the Operator role'
			)
		);
		mount();
		const alert = await screen.findByRole('alert');
		expect(alert.textContent).toMatch(/needs the Operator role/);
		expect(screen.queryByText('No approvals pending')).toBeNull();
	});

	it('outside a share the commit carries only the draft id', async () => {
		share.current = null;
		vi.mocked(paActionsCommit).mockResolvedValue(undefined);
		mount();
		expect(screen.queryByRole('note')).toBeNull();
		await approve();
		await waitFor(() => expect(paActionsCommit).toHaveBeenCalledTimes(1));
		expect(vi.mocked(paActionsCommit).mock.calls[0]).toEqual([ROW.id]);
	});
});
