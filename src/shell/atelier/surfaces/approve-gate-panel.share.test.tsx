/* Share-mode rules of <ApproveGatePanel /> (box plan WP-5): the note, the row that was
 * shown travelling with the approval, unsaved edits blocking it, a failed action
 * coming back onto the draft, and the desktop path staying exactly as it was. */

import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { isMacPlatform } from '@/lib/keymap/platform';
import type { PausedDraftView } from '@/lib/queries/pa-actions';
import { ApproveGatePanel } from './approve-gate-panel';
import { APPROVE_GATE_FIXTURES } from './approve-gate-panel.fixtures';

afterEach(cleanup);

const SEEN = {
	channel: 'smtp',
	payloadJson: '{"item":{"subject":"s"}}',
	editedJson: null,
	scheduledAt: null,
};
const DRAFTS: PausedDraftView[] = APPROVE_GATE_FIXTURES.map((d) => ({ ...d, seen: SEEN }));
const FIRST = DRAFTS[0].id;

function runUndoWindow() {
	for (let i = 0; i < 12; i++) {
		act(() => {
			vi.advanceTimersByTime(1000);
		});
	}
}

describe('ApproveGatePanel in a share', () => {
	it('shows the share note; the desktop panel shows none', () => {
		const { rerender } = render(
			<ApproveGatePanel
				drafts={DRAFTS}
				onApprove={vi.fn()}
				onReject={vi.fn()}
				shareNote="Approving here sends from the box."
			/>
		);
		expect(screen.getByRole('note').textContent).toBe('Approving here sends from the box.');
		rerender(<ApproveGatePanel drafts={DRAFTS} onApprove={vi.fn()} onReject={vi.fn()} />);
		expect(screen.queryByRole('note')).toBeNull();
	});

	it('approves with the row that was shown, captured when Approve was pressed', () => {
		vi.useFakeTimers();
		try {
			const onApprove = vi.fn();
			render(
				<ApproveGatePanel drafts={DRAFTS} onApprove={onApprove} onReject={vi.fn()} shareNote="n" />
			);
			fireEvent.click(screen.getByRole('button', { name: /Approve & Send/ }));
			runUndoWindow();
			expect(onApprove).toHaveBeenCalledTimes(1);
			expect(onApprove).toHaveBeenCalledWith(FIRST, SEEN);
		} finally {
			vi.useRealTimers();
		}
	});

	it('does not start an approval over unsaved edits, and says why', () => {
		const onApprove = vi.fn();
		render(
			<ApproveGatePanel drafts={DRAFTS} onApprove={onApprove} onReject={vi.fn()} shareNote="n" />
		);
		fireEvent.change(screen.getByLabelText('Email subject'), { target: { value: 'Changed' } });
		fireEvent.click(screen.getByRole('button', { name: /Approve & Send/ }));
		expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
		expect(screen.getByRole('alert', { name: 'Approval error' }).textContent).toMatch(
			/unsaved edits/
		);
		expect(onApprove).not.toHaveBeenCalled();
	});

	it('shows a failed approval on the draft and brings a removed row back', () => {
		vi.useFakeTimers();
		try {
			const onApprove = vi.fn();
			const { rerender } = render(
				<ApproveGatePanel drafts={DRAFTS} onApprove={onApprove} onReject={vi.fn()} shareNote="n" />
			);
			fireEvent.click(screen.getByRole('button', { name: /Approve & Send/ }));
			runUndoWindow();
			// The panel removed the row optimistically when approve fired.
			expect(screen.queryByRole('alert', { name: 'Approval error' })).toBeNull();
			expect(screen.queryAllByRole('button', { name: /Valentim de Carvalho/ })).toHaveLength(0);
			rerender(
				<ApproveGatePanel
					drafts={DRAFTS}
					onApprove={onApprove}
					onReject={vi.fn()}
					shareNote="n"
					draftErrors={{ [FIRST]: 'Nothing was approved: the box has no such draft.' }}
				/>
			);
			const alert = screen.getByRole('alert', { name: 'Approval error' });
			expect(alert.textContent).toContain('Not approved');
			expect(alert.textContent).toContain('Nothing was approved: the box has no such draft.');
			expect(screen.getAllByText('NOT APPROVED').length).toBeGreaterThan(0);
			expect(
				screen.getAllByRole('button', { name: /Valentim de Carvalho/ }).length
			).toBeGreaterThan(0);
		} finally {
			vi.useRealTimers();
		}
	});

	it('desktop: no share note, approve carries only the id, edits are not blocked', () => {
		vi.useFakeTimers();
		try {
			const onApprove = vi.fn();
			render(<ApproveGatePanel drafts={DRAFTS} onApprove={onApprove} onReject={vi.fn()} />);
			fireEvent.change(screen.getByLabelText('Email subject'), { target: { value: 'Changed' } });
			fireEvent.click(screen.getByRole('button', { name: /Approve & Send/ }));
			runUndoWindow();
			expect(onApprove).toHaveBeenCalledWith(FIRST);
			expect(onApprove.mock.calls[0]).toHaveLength(1);
		} finally {
			vi.useRealTimers();
		}
	});
});

// ── what the viewer sees is what the approval binds ───────────────────────────────────────────

const MOD = isMacPlatform() ? { metaKey: true } : { ctrlKey: true };
const bodyBox = () => screen.getByLabelText('Email body') as HTMLTextAreaElement;

/** The first fixture as the box's row `rowBody`: the draft view and the `seen` row agree. */
function rowWithBody(rowBody: string): PausedDraftView {
	return {
		...DRAFTS[0],
		body: rowBody,
		seen: { ...SEEN, editedJson: JSON.stringify({ body: rowBody }) },
	};
}

function approveFirst(onApprove: ReturnType<typeof vi.fn>) {
	fireEvent.click(screen.getByRole('button', { name: /Approve & Send/ }));
	runUndoWindow();
	return onApprove;
}

describe('ApproveGatePanel in a share: the screen is the row the approval binds', () => {
	it('shows the box’s row, not the viewer’s old edit, once the agent rewrote the draft after a save', () => {
		vi.useFakeTimers();
		try {
			const onApprove = vi.fn();
			const onEdit = vi.fn();
			const props = { onApprove, onReject: vi.fn(), onEdit, shareNote: 'n' };
			const { rerender } = render(
				<ApproveGatePanel drafts={[rowWithBody('ORIGINAL')]} {...props} />
			);
			fireEvent.change(bodyBox(), { target: { value: 'ADA BODY' } });
			fireEvent.keyDown(bodyBox(), { key: 's', ...MOD });
			expect(onEdit).toHaveBeenCalledWith(FIRST, expect.objectContaining({ body: 'ADA BODY' }));
			// The agent, which owns the database, overwrites the draft; the 15 s refetch brings it in.
			rerender(<ApproveGatePanel drafts={[rowWithBody('AGENT: wire money to EVIL')]} {...props} />);
			expect(bodyBox().value).toBe('AGENT: wire money to EVIL');
			expect(screen.queryByDisplayValue('ADA BODY')).toBeNull();
			approveFirst(onApprove);
			const seen = onApprove.mock.calls[0][1];
			expect(JSON.parse(seen.editedJson).body).toBe('AGENT: wire money to EVIL');
		} finally {
			vi.useRealTimers();
		}
	});

	it('does not approve a saved edit until the box has returned it', () => {
		vi.useFakeTimers();
		try {
			const onApprove = vi.fn();
			const props = { onApprove, onReject: vi.fn(), onEdit: vi.fn(), shareNote: 'n' };
			const { rerender } = render(
				<ApproveGatePanel drafts={[rowWithBody('ORIGINAL')]} {...props} />
			);
			fireEvent.change(bodyBox(), { target: { value: 'ADA BODY' } });
			fireEvent.keyDown(bodyBox(), { key: 's', ...MOD });
			// Saved, but the list has not been refetched: the row is still the old one.
			fireEvent.click(screen.getByRole('button', { name: /Approve & Send/ }));
			expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
			expect(screen.getByRole('alert', { name: 'Approval error' }).textContent).toMatch(
				/not come back from the box/
			);
			// The refetch returns exactly what was saved: now the screen is the row.
			rerender(<ApproveGatePanel drafts={[rowWithBody('ADA BODY')]} {...props} />);
			expect(bodyBox().value).toBe('ADA BODY');
			approveFirst(onApprove);
			expect(JSON.parse(onApprove.mock.calls[0][1].editedJson).body).toBe('ADA BODY');
		} finally {
			vi.useRealTimers();
		}
	});

	it('typing after a save makes the edit unsaved again', () => {
		const onApprove = vi.fn();
		const props = { onApprove, onReject: vi.fn(), onEdit: vi.fn(), shareNote: 'n' };
		const { rerender } = render(<ApproveGatePanel drafts={[rowWithBody('ORIGINAL')]} {...props} />);
		fireEvent.change(bodyBox(), { target: { value: 'ADA BODY' } });
		fireEvent.keyDown(bodyBox(), { key: 's', ...MOD });
		rerender(<ApproveGatePanel drafts={[rowWithBody('ADA BODY')]} {...props} />);
		fireEvent.change(bodyBox(), { target: { value: 'ADA BODY, and more' } });
		fireEvent.click(screen.getByRole('button', { name: /Approve & Send/ }));
		expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
		expect(screen.getByRole('alert', { name: 'Approval error' }).textContent).toMatch(
			/unsaved edits/
		);
		expect(bodyBox().value).toBe('ADA BODY, and more');
	});
});

describe('ApproveGatePanel in a share: every field the hash covers is shown', () => {
	const ROUTED = {
		channel: 'smtp',
		payloadJson: JSON.stringify({
			item: {
				recipient: 'Pat Customer',
				recipientEmail: 'pat@example.com',
				recipientsList: [{ email: 'exfil@evil.test' }],
				cc: ['exfil@evil.test'],
				replyTo: 'attacker@evil.test',
				in_reply_to: '<abc@evil.test>',
				subject: 's',
				body: 'b',
			},
		}),
		editedJson: JSON.stringify({ subject: 's2', hidden: 'only-in-the-edit' }),
		scheduledAt: '2026-10-21 09:00:00',
	};

	it('lists recipients, cc, reply-to, threading headers, other edited keys, channel and schedule', () => {
		render(
			<ApproveGatePanel
				drafts={[{ ...DRAFTS[0], seen: ROUTED }]}
				onApprove={vi.fn()}
				onReject={vi.fn()}
				shareNote="n"
			/>
		);
		const region = screen.getByRole('region', { name: 'Approval covers' });
		const text = region.textContent ?? '';
		for (const needle of [
			'recipientsList[0].email',
			'cc[0]',
			'replyTo',
			'attacker@evil.test',
			'in_reply_to',
			'<abc@evil.test>',
			'hidden',
			'only-in-the-edit',
			'2026-10-21 09:00:00',
			'smtp',
		]) {
			expect(text).toContain(needle);
		}
		expect(text).toContain('exfil@evil.test');
	});

	it('shows nothing of the kind on the desktop', () => {
		render(
			<ApproveGatePanel
				drafts={[{ ...DRAFTS[0], seen: ROUTED }]}
				onApprove={vi.fn()}
				onReject={vi.fn()}
			/>
		);
		expect(screen.queryByRole('region', { name: 'Approval covers' })).toBeNull();
	});

	it('refuses to approve a row whose edit cannot be read', () => {
		const onApprove = vi.fn();
		render(
			<ApproveGatePanel
				drafts={[{ ...DRAFTS[0], seen: { ...ROUTED, editedJson: '{"body":"x"' } }]}
				onApprove={onApprove}
				onReject={vi.fn()}
				shareNote="n"
			/>
		);
		fireEvent.click(screen.getByRole('button', { name: /Approve & Send/ }));
		expect(screen.queryByRole('button', { name: 'Undo' })).toBeNull();
		expect(screen.getByRole('alert', { name: 'Approval error' }).textContent).toMatch(
			/cannot be approved/
		);
		expect(onApprove).not.toHaveBeenCalled();
	});

	it('offers no bulk approve in a share', () => {
		render(
			<ApproveGatePanel drafts={DRAFTS} onApprove={vi.fn()} onReject={vi.fn()} shareNote="n" />
		);
		fireEvent.click(screen.getAllByRole('checkbox', { name: /Select .* draft/ })[0]);
		expect(screen.queryByRole('button', { name: 'Approve all' })).toBeNull();
		expect(screen.getByRole('button', { name: 'Reject all' })).toBeTruthy();
	});

	it('does not crash on a draft whose recipients is a list', () => {
		const d = {
			...DRAFTS[0],
			consequence: {
				...DRAFTS[0].consequence,
				recipients: [{ email: 'a@b.c' }, { email: 'd@e.f' }],
			},
		} as unknown as PausedDraftView;
		render(<ApproveGatePanel drafts={[d]} onApprove={vi.fn()} onReject={vi.fn()} shareNote="n" />);
		expect(screen.getAllByText(/2 recipients/).length).toBeGreaterThan(0);
	});
});
