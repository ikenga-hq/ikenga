/* Render + a11y + interaction tests for <ApproveGatePanel />.
 * Covers the queue render, the accessible-name contract (the 5 quality-gate blockers),
 * J/K row navigation, ⌘S edit, the 10s undo window, and the reject seam. */

import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { isMacPlatform } from '@/lib/keymap/platform';
import {
	deriveWorkerHealth,
	type PausedDraftView,
	type WorkerHealth,
} from '@/lib/queries/pa-actions';
import type { PaActionDraftRow } from '@/lib/tauri-cmd';
import { ApproveGatePanel } from './approve-gate-panel';
import { APPROVE_GATE_FIXTURES } from './approve-gate-panel.fixtures';

afterEach(cleanup);

// WP-56: J/K and ⌘S/⌘↵ now fire through the registry (`approve-gate.*`,
// `approveGateFocus`), which resolves `mod` per-platform (§3.1) rather than
// the old handler's platform-agnostic `e.metaKey || e.ctrlKey`.
const MOD = isMacPlatform() ? { metaKey: true } : { ctrlKey: true };

function setup(
	over: Partial<Record<'onApprove' | 'onReject' | 'onEdit', ReturnType<typeof vi.fn>>> = {}
) {
	const h = {
		onApprove: vi.fn(),
		onReject: vi.fn(),
		onEdit: vi.fn(),
		...over,
	};
	render(<ApproveGatePanel drafts={APPROVE_GATE_FIXTURES} {...h} />);
	return h;
}

const subjectValue = () => (screen.getByLabelText('Email subject') as HTMLInputElement).value;

describe('ApproveGatePanel', () => {
	it('renders the draft queue from fixtures', () => {
		setup();
		expect(screen.getByText('Approvals')).toBeTruthy();
		expect(screen.getAllByText('Valentim de Carvalho').length).toBeGreaterThan(0);
		expect(screen.getByText('Hannah Ekudo')).toBeTruthy();
		expect(screen.getByText('L5 Winback · 388 recipients')).toBeTruthy();
		expect(screen.getByText(/1 overdue/)).toBeTruthy();
	});

	it('exposes accessible names on rows, checkboxes, fields, and the divider (a11y blockers)', () => {
		setup();
		// blocker 3 — editable fields have programmatic labels
		expect(screen.getByLabelText('Email subject')).toBeTruthy();
		expect(screen.getByLabelText('Email body')).toBeTruthy();
		// rows + checkboxes
		expect(
			screen.getByRole('button', { name: /Valentim de Carvalho – Re: Catalog import/ })
		).toBeTruthy();
		expect(
			screen.getByRole('checkbox', { name: 'Select Valentim de Carvalho draft' })
		).toBeTruthy();
		// primary CTA carries the keyboard shortcut
		expect(
			screen.getByRole('button', { name: /Approve & Send/ }).getAttribute('aria-keyshortcuts')
		).toBe('Meta+Enter');
		// divider is an operable separator
		expect(screen.getByRole('separator', { name: 'Resize draft list' })).toBeTruthy();
		// the section landmark
		expect(screen.getByLabelText('Draft approvals')).toBeTruthy();
	});

	it('J/K navigates the queue in display order (blocker 5)', () => {
		setup();
		const surface = screen.getByLabelText('Draft approvals');
		// initial selection = Valentim (Today). Display order: Overdue(Hannah) → Today(Valentim, LinkedIn) → This week(L5).
		expect(subjectValue()).toMatch(/Re: Catalog import/);
		fireEvent.keyDown(surface, { key: 'j' }); // next (j = down, vim) → LinkedIn social
		expect(subjectValue()).toMatch(/Ship note/);
		fireEvent.keyDown(surface, { key: 'k' }); // previous (k = up, vim) → Valentim
		expect(subjectValue()).toMatch(/Re: Catalog import/);
	});

	it('⌘S persists an edit and shows the saved indicator + EDITED chip', () => {
		const h = setup();
		fireEvent.change(screen.getByLabelText('Email subject'), {
			target: { value: 'Re: Catalog import — updated' },
		});
		fireEvent.keyDown(screen.getByLabelText('Email body'), { key: 's', ...MOD });
		expect(h.onEdit).toHaveBeenCalled();
		expect(screen.getByText(/Saved/)).toBeTruthy();
	});

	it('Approve & Send opens the undo window; Undo cancels before commit', () => {
		const h = setup();
		fireEvent.click(screen.getByRole('button', { name: /Approve & Send/ }));
		// the Undo button exists only inside the undo toast → its presence proves the window opened
		expect(screen.getByRole('button', { name: 'Undo' })).toBeTruthy();
		fireEvent.click(screen.getByRole('button', { name: 'Undo' }));
		expect(h.onApprove).not.toHaveBeenCalled();
	});

	it('commits onApprove after the 10s undo window elapses', () => {
		vi.useFakeTimers();
		try {
			const h = setup();
			fireEvent.click(screen.getByRole('button', { name: /Approve & Send/ }));
			for (let i = 0; i < 12; i++) {
				act(() => {
					vi.advanceTimersByTime(1000);
				});
			}
			expect(h.onApprove).toHaveBeenCalledWith('4f8e2');
		} finally {
			vi.useRealTimers();
		}
	});

	it('Reject fires onReject and removes the row', () => {
		const h = setup();
		fireEvent.click(screen.getByRole('button', { name: 'Reject' }));
		expect(h.onReject).toHaveBeenCalledWith('4f8e2');
		expect(screen.queryByText('Valentim de Carvalho')).toBeNull();
	});

	it('checking rows reveals the bulk bar and Approve all commits the checked set', () => {
		const h = setup();
		fireEvent.click(screen.getByRole('checkbox', { name: 'Select Hannah Ekudo draft' }));
		fireEvent.click(screen.getByRole('button', { name: 'Approve all' }));
		expect(h.onApprove).toHaveBeenCalledWith('rs-91c2');
	});
});

// ── WP-11 · worker-liveness derivation ────────────────────────────────────────────────────────

const NOW = Date.parse('2026-07-03T12:00:00.000Z');
const MIN = 60_000;
const ago = (ms: number) => new Date(NOW - ms).toISOString();

/** Minimal `pa_action_drafts` row with committed/due defaults; override per case. */
function makeRow(over: Partial<PaActionDraftRow> = {}): PaActionDraftRow {
	return {
		id: 'r1',
		batchId: 'b1',
		actionId: 'a1',
		status: 'committed',
		channel: 'smtp',
		payloadJson: '{}',
		editedJson: null,
		scheduledAt: null,
		createdAt: ago(30_000),
		committedAt: ago(30_000),
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

describe('deriveWorkerHealth (WP-11 liveness legend)', () => {
	it('reads ALIVE when the worker is touching rows and the queue is fresh', () => {
		const rows = [
			makeRow({ id: 's', status: 'sending', claimedAt: ago(20_000), lastAttemptAt: ago(20_000) }),
			makeRow({ id: 'q', status: 'committed', committedAt: ago(30_000) }),
		];
		const h = deriveWorkerHealth(rows, NOW);
		expect(h.state).toBe('alive');
		expect(h.sending).toBe(1);
		expect(h.queued).toBe(1);
		expect(h.lastActivityMsAgo).toBe(20_000);
	});

	it('reads DEGRADED when a due committed row ages past T_expect while the worker is still live', () => {
		const rows = [
			makeRow({ status: 'committed', committedAt: ago(6 * MIN), claimedAt: ago(1 * MIN) }),
		];
		const h = deriveWorkerHealth(rows, NOW);
		expect(h.state).toBe('degraded');
		expect(h.queued).toBe(1);
		expect(h.oldestQueuedMsAgo).toBe(6 * MIN);
	});

	it('reads DEGRADED when failures pile up within the trailing hour', () => {
		const rows = [
			makeRow({ id: 'f1', status: 'failed', lastAttemptAt: ago(10 * MIN), attempts: 2 }),
			makeRow({ id: 'f2', status: 'failed', lastAttemptAt: ago(20 * MIN), attempts: 1 }),
		];
		const h = deriveWorkerHealth(rows, NOW);
		expect(h.state).toBe('degraded');
		expect(h.failed).toBe(2);
		expect(h.failuresThisHour).toBe(2);
	});

	it('reads DEAD when a due backlog exists but nothing has worker activity (all NULL)', () => {
		const rows = [makeRow({ status: 'committed', committedAt: ago(20 * MIN) })];
		const h = deriveWorkerHealth(rows, NOW);
		expect(h.state).toBe('dead');
		expect(h.queued).toBe(1);
		expect(h.lastActivityMsAgo).toBeNull();
	});

	it('reads DEAD when the last worker activity is older than T_dead', () => {
		const rows = [
			makeRow({ status: 'committed', committedAt: ago(20 * MIN), claimedAt: ago(18 * MIN) }),
		];
		expect(deriveWorkerHealth(rows, NOW).state).toBe('dead');
	});

	it('does not count future-scheduled committed rows as due backlog', () => {
		const rows = [
			makeRow({
				status: 'committed',
				committedAt: ago(1 * MIN),
				scheduledAt: new Date(NOW + 60 * MIN).toISOString(),
			}),
		];
		const h = deriveWorkerHealth(rows, NOW);
		expect(h.scheduled).toBe(1);
		expect(h.queued).toBe(0);
		expect(h.state).toBe('idle'); // no due backlog + no heartbeat ⇒ honest idle, not green alive
	});

	it('reads IDLE (not alive) when activity is stale but nothing is due — the 30h-ago case', () => {
		// Visual-review finding: 4 old failures, last attempt 30h ago, empty queue
		// rendered a green "Alive" beside "last worker activity 30h ago".
		const rows = [
			makeRow({ id: 'f1', status: 'failed', lastAttemptAt: ago(30 * 60 * MIN), attempts: 2 }),
			makeRow({ id: 'f2', status: 'failed', lastAttemptAt: ago(31 * 60 * MIN), attempts: 5 }),
		];
		const h = deriveWorkerHealth(rows, NOW);
		expect(h.failed).toBe(2);
		expect(h.failuresThisHour).toBe(0);
		expect(h.queued).toBe(0);
		expect(h.state).toBe('idle');
	});
});

// ── WP-11 · delivery chips + health strip (panel rendering) ───────────────────────────────────

const aliveHealth: WorkerHealth = {
	state: 'alive',
	sending: 0,
	queued: 1,
	failed: 0,
	failuresThisHour: 0,
	scheduled: 0,
	lastActivityAt: ago(20_000),
	lastActivityMsAgo: 20_000,
	oldestQueuedMsAgo: 40_000,
};
const degradedHealth: WorkerHealth = {
	state: 'degraded',
	sending: 0,
	queued: 1,
	failed: 1,
	failuresThisHour: 2,
	scheduled: 0,
	lastActivityAt: ago(1 * MIN),
	lastActivityMsAgo: 1 * MIN,
	oldestQueuedMsAgo: 6 * MIN,
};
const deadHealth: WorkerHealth = {
	state: 'dead',
	sending: 0,
	queued: 3,
	failed: 0,
	failuresThisHour: 0,
	scheduled: 0,
	lastActivityAt: null,
	lastActivityMsAgo: null,
	oldestQueuedMsAgo: 18 * MIN,
};

/** Build a `PausedDraftView` by cloning a fixture and stamping delivery/display state. */
function view(over: Partial<PausedDraftView> & { id: string }): PausedDraftView {
	return { ...APPROVE_GATE_FIXTURES[0], ...over };
}

function renderPanel(drafts: PausedDraftView[], health?: WorkerHealth) {
	const h = {
		onApprove: vi.fn(),
		onReject: vi.fn(),
		onEdit: vi.fn(),
		onRetry: vi.fn(),
	};
	render(<ApproveGatePanel drafts={drafts} health={health} {...h} />);
	return h;
}

describe('ApproveGatePanel delivery chips (WP-11)', () => {
	it('renders a sending chip with the attempt count', () => {
		renderPanel([
			view({
				id: 's1',
				status: 'awaiting',
				section: 'Today',
				delivery: {
					dbStatus: 'sending',
					claimedAt: ago(20_000),
					committedAt: ago(60_000),
					sentAt: null,
					lastAttemptAt: ago(20_000),
					externalId: null,
					deliveryStatus: null,
					deliveryCheckedAt: null,
					attempts: 1,
					scheduledAt: null,
				},
			}),
		]);
		expect(screen.getByText(/sending · attempt 1/)).toBeTruthy();
	});

	it('splits a committed row into queued (alive) vs stalled (dead)', () => {
		// The chip's age label is computed against the real Date.now() (deliveryChipLabel
		// takes no injectable clock), so the committed timestamp must be real-now relative.
		const draft = view({
			id: 'q1',
			status: 'awaiting',
			section: 'Today',
			delivery: {
				dbStatus: 'committed',
				claimedAt: null,
				committedAt: new Date(Date.now() - 40_000).toISOString(),
				sentAt: null,
				lastAttemptAt: null,
				externalId: null,
				deliveryStatus: null,
				deliveryCheckedAt: null,
				attempts: 0,
				scheduledAt: null,
			},
		});
		// No health ⇒ chip reads "queued"; no strip rendered.
		renderPanel([draft]);
		expect(screen.getByText(/queued · \d+s/)).toBeTruthy();
		cleanup();
		// Dead worker ⇒ same committed row reads "no worker" (stalled).
		renderPanel([draft], deadHealth);
		expect(screen.getByText(/no worker · \d+s/)).toBeTruthy();
	});

	it('renders a sent chip with its delivery_status sub-chip', () => {
		renderPanel([
			view({
				id: 'snt',
				status: 'awaiting',
				section: 'Today',
				delivery: {
					dbStatus: 'sent',
					claimedAt: ago(5 * MIN),
					committedAt: ago(6 * MIN),
					sentAt: ago(3 * MIN),
					lastAttemptAt: ago(3 * MIN),
					externalId: 're_123',
					deliveryStatus: 'delivered',
					deliveryCheckedAt: ago(2 * MIN),
					attempts: 1,
					scheduledAt: null,
				},
			}),
		]);
		expect(screen.getByText(/delivered · checked \d\d?:\d\d/)).toBeTruthy();
	});

	it('opens the error popover on a failed chip and Retry reuses the onRetry seam', () => {
		const h = renderPanel([
			view({
				id: 'f1',
				status: 'failed',
				section: 'Overdue',
				errorMessage: 'Resend 429: rate_limit_exceeded — retry after 60s',
				attempts: 3,
				delivery: {
					dbStatus: 'failed',
					claimedAt: ago(3 * MIN),
					committedAt: ago(5 * MIN),
					sentAt: null,
					lastAttemptAt: ago(1 * MIN),
					externalId: null,
					deliveryStatus: null,
					deliveryCheckedAt: null,
					attempts: 3,
					scheduledAt: null,
				},
			}),
		]);
		const chip = screen.getByRole('button', { name: /Send failed, 3 attempts/ });
		expect(chip.textContent).toMatch(/failed · 3×/);
		fireEvent.click(chip);
		const dialog = screen.getByRole('dialog', { name: 'Send error detail' });
		expect(within(dialog).getByText(/rate_limit_exceeded/)).toBeTruthy();
		fireEvent.click(within(dialog).getByRole('button', { name: /Retry/ }));
		expect(h.onRetry).toHaveBeenCalledWith('f1');
	});
});

describe('ApproveGatePanel health strip (WP-11)', () => {
	const queuedDraft = view({
		id: 'q1',
		status: 'awaiting',
		section: 'Today',
		delivery: {
			dbStatus: 'committed',
			claimedAt: null,
			committedAt: ago(40_000),
			sentAt: null,
			lastAttemptAt: null,
			externalId: null,
			deliveryStatus: null,
			deliveryCheckedAt: null,
			attempts: 0,
			scheduledAt: null,
		},
	});

	it('shows the Alive state as a status region', () => {
		renderPanel([queuedDraft], aliveHealth);
		expect(screen.getByRole('status', { name: 'Send worker health' })).toBeTruthy();
		expect(screen.getByText('Alive')).toBeTruthy();
	});

	it('shows the Degraded state', () => {
		renderPanel([queuedDraft], degradedHealth);
		expect(screen.getByText('Degraded')).toBeTruthy();
	});

	it('escalates the Dead state to an alert with the diagnose affordance', () => {
		renderPanel([queuedDraft], deadHealth);
		expect(screen.getByRole('alert', { name: 'Send worker health' })).toBeTruthy();
		expect(screen.getByText('No signal')).toBeTruthy();
		expect(screen.getByRole('button', { name: /Copy diagnose cmd/ })).toBeTruthy();
		expect(screen.getByText(/Approved drafts will not send until the worker is back/)).toBeTruthy();
	});
});
