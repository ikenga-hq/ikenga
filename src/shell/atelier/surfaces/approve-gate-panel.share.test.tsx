/* Share-mode rules of <ApproveGatePanel /> (box plan WP-5): the note, the row that was
 * shown travelling with the approval, unsaved edits blocking it, a failed action
 * coming back onto the draft, and the desktop path staying exactly as it was. */

import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
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
