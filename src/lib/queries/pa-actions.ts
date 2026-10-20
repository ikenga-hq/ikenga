import {
	type ApproveGateMeta,
	type DraftItem,
	fromDraftItem,
	type PausedDraft,
} from '@ikenga/contract';
import { queryOptions } from '@tanstack/react-query';
import { queryKeys } from '@/lib/query-keys';
import { type PaActionDraftRow, type PaDraftSeen, paActionsList } from '@/lib/tauri-cmd';

/** TanStack Query options for the gate's active draft set (or a status filter). */
export function paActionsListQueryOptions(status?: string) {
	return queryOptions({
		queryKey: queryKeys.paActions.list(status),
		queryFn: () => paActionsList(status),
		staleTime: 10_000,
	});
}

/**
 * Parse one `pa_action_drafts` row into the panel's `PausedDraft` view-model.
 *
 * Producer contract: `payloadJson` is `{ item: DraftItem, meta: ApproveGateMeta }`
 * (the producer writes it via `host.paActionsPause`); `editedJson` is the
 * operator's `{ subject?, body? }` overrides. Returns `null` on malformed JSON so
 * the caller can skip the row rather than crash the gate.
 *
 * WP-12 / G-09: reads `row.status`, `row.errorText`, and `row.attempts` from the
 * DB row and stamps them onto the view-model so the panel can surface failed rows
 * with their error callout and Retry button. `fromDraftItem` derives its own
 * overdue/today status from the scheduled time; we override with `'failed'` only
 * when the DB row is actually in the `failed` state.
 */
export function pausedDraftFromRow(row: PaActionDraftRow): PausedDraft | null {
	let parsed: { item?: DraftItem; meta?: ApproveGateMeta };
	try {
		parsed = JSON.parse(row.payloadJson) as { item?: DraftItem; meta?: ApproveGateMeta };
	} catch {
		return null;
	}
	if (!parsed?.item || !parsed?.meta) return null;

	let item = parsed.item;
	let edited = false;
	if (row.editedJson) {
		try {
			const patch = JSON.parse(row.editedJson) as { subject?: string; body?: string };
			item = {
				...item,
				subject: patch.subject ?? item.subject,
				body: patch.body ?? item.body,
			};
			edited = true;
		} catch {
			// malformed edit patch — fall back to the original draft
		}
	}

	const draft = fromDraftItem(item, parsed.meta);
	if (edited) {
		draft.everEdited = true;
		if (draft.status === 'awaiting') draft.status = 'edited';
	}

	// WP-12 / G-09 — propagate DB-side status onto the view-model.
	// `failed` is the only status that originates from the worker (not the FE) and
	// needs to override fromDraftItem's derived status.
	if (row.status === 'failed') {
		draft.status = 'failed';
	}
	// Surface worker-written error context so the panel can render the callout.
	if (row.errorText != null) {
		draft.errorMessage = row.errorText;
	}
	if (row.attempts > 0) {
		draft.attempts = row.attempts;
	}

	// WP-11 — carry the worker-authored delivery columns through so the panel can
	// render per-row delivery chips (queued/sending/sent/failed/stalled) and the
	// error popover without a second query. All fields are migration-0051 columns
	// already on `row`; this is a pure mapping change (design grounding note).
	const view = draft as PausedDraftView;
	// THE ROW'S PRIMARY KEY IS THE ONLY ID THE HOST COMMANDS ACCEPT.
	// `fromDraftItem` derives its id from the *payload*, which is not the
	// `pa_action_drafts` key, so without this every action button sent
	// `{ draftId: undefined }` — and an undefined value serialises to an ABSENT
	// key, so Tauri rejected it with "missing required key draftId" rather than
	// with a not-found. That silently broke Approve, Reject AND Retry: the panel
	// optimistically marked the row resolved and removed it, so the gate looked
	// like it had worked while nothing was ever written.
	view.id = row.id;
	// The row exactly as listed: a share approval binds to these bytes.
	view.seen = {
		channel: row.channel,
		payloadJson: row.payloadJson,
		editedJson: row.editedJson,
		scheduledAt: row.scheduledAt,
	};
	view.delivery = {
		dbStatus: row.status,
		claimedAt: row.claimedAt,
		committedAt: row.committedAt,
		sentAt: row.sentAt,
		lastAttemptAt: row.lastAttemptAt,
		externalId: row.externalId,
		deliveryStatus: row.deliveryStatus,
		deliveryCheckedAt: row.deliveryCheckedAt,
		attempts: row.attempts,
		scheduledAt: row.scheduledAt,
	};
	return view;
}

// ── Delivery health (WP-11) ─────────────────────────────────────────────────

/**
 * Worker-authored send-state carried from a `pa_action_drafts` row onto the
 * panel view-model. Every field exists on migration 0051 and already reaches the
 * FE on `PaActionDraftRow`; `pausedDraftFromRow` copies them through verbatim.
 */
export interface DraftDelivery {
	/** Raw `pa_action_drafts.status` — the worker lifecycle state
	 *  (awaiting|edited|committed|sending|sent|failed|rejected). Distinct from the
	 *  panel's display `status`, which collapses to awaiting|edited|overdue|failed. */
	dbStatus: string;
	claimedAt: string | null;
	committedAt: string | null;
	sentAt: string | null;
	lastAttemptAt: string | null;
	externalId: string | null;
	/** null | accepted | delivered | bounced | complained | errored */
	deliveryStatus: string | null;
	deliveryCheckedAt: string | null;
	attempts: number;
	/** Machine schedule time — drives the due-vs-scheduled split for the chip. */
	scheduledAt: string | null;
}

/** `PausedDraft` plus the worker-written delivery columns. Optional because
 *  fixtures and pre-worker rows omit it (the chip simply doesn't render). */
export type PausedDraftView = PausedDraft & { delivery?: DraftDelivery; seen?: PaDraftSeen };

/** The text to show on a draft for a failed approve / reject / edit. Share
 *  refusals arrive as `<code>: <sentence>`; show the sentence. */
export function draftActionErrorMessage(err: unknown): string {
	const raw = err instanceof Error ? err.message : typeof err === 'string' ? err : '';
	const m =
		/^(?:approval_unavailable|approval_refused|draft_changed|forbidden|invalid_request): (.+)$/s.exec(
			raw
		);
	const text = (m ? m[1] : raw).trim();
	return text || 'The action failed. Nothing was changed.';
}

export type WorkerHealthState = 'alive' | 'idle' | 'degraded' | 'dead';

/**
 * Client-side worker-liveness snapshot derived purely from the existing
 * `paActionsList` query — no new IPC, no daemon heartbeat (the shell has no
 * channel to the external send-worker, so death is *inferred* from committed
 * rows nobody claims). Legend + predicates: parity-delivery-health.html §2.
 */
export interface WorkerHealth {
	state: WorkerHealthState;
	/** rows in `status='sending'` right now. */
	sending: number;
	/** committed & due rows waiting for the worker to claim (the backlog). */
	queued: number;
	/** rows the worker gave up on (`status='failed'`). */
	failed: number;
	/** failed rows whose last attempt was within the trailing hour. */
	failuresThisHour: number;
	/** committed rows scheduled for the future (not part of the due backlog). */
	scheduled: number;
	/** max(claimed_at, last_attempt_at) across the set — the only heartbeat we have. */
	lastActivityAt: string | null;
	lastActivityMsAgo: number | null;
	/** age of the oldest due committed row (queue-aging signal). */
	oldestQueuedMsAgo: number | null;
}

/** Queue-aging expectation: a due committed row older than this, while the worker
 *  is still touching rows, reads as degraded. */
const T_EXPECT_MS = 2 * 60_000;
/** No-signal threshold: a due backlog with no worker activity this long reads as
 *  dead. Must exceed the worker's poll interval + worst-case single-send wall
 *  time (0051 ties the reaper to the job's timeout_ms) — 15m is a placeholder
 *  pending the founder's actual poll cadence (design open question). */
const T_DEAD_MS = 15 * 60_000;
/** Failures within the trailing hour that flip a live worker to degraded. */
const DEGRADED_FAILURES = 2;
const ONE_HOUR_MS = 60 * 60_000;

/**
 * Derive worker liveness from the draft rows. States resolve by priority
 * (dead → degraded → alive) so the overlapping predicates in the legend collapse
 * to the most actionable reading. Pure + `now`-injectable for tests.
 */
export function deriveWorkerHealth(
	rows: PaActionDraftRow[],
	now: number = Date.now()
): WorkerHealth {
	const parse = (s: string | null): number => (s ? Date.parse(s) : Number.NaN);

	let sending = 0;
	let queued = 0;
	let failed = 0;
	let failuresThisHour = 0;
	let scheduled = 0;
	let lastActivity = Number.NEGATIVE_INFINITY;
	let oldestDueCommitted = Number.POSITIVE_INFINITY;

	for (const r of rows) {
		const claimed = parse(r.claimedAt);
		const lastAttempt = parse(r.lastAttemptAt);
		if (!Number.isNaN(claimed)) lastActivity = Math.max(lastActivity, claimed);
		if (!Number.isNaN(lastAttempt)) lastActivity = Math.max(lastActivity, lastAttempt);

		const sched = parse(r.scheduledAt);
		// A null schedule is due immediately (the worker's own claim predicate).
		const due = Number.isNaN(sched) || sched <= now;

		switch (r.status) {
			case 'sending':
				sending++;
				break;
			case 'failed':
				failed++;
				if (!Number.isNaN(lastAttempt) && now - lastAttempt <= ONE_HOUR_MS) failuresThisHour++;
				break;
			case 'committed':
				if (due) {
					queued++;
					const committed = parse(r.committedAt);
					const anchor = Number.isNaN(committed) ? parse(r.createdAt) : committed;
					if (!Number.isNaN(anchor)) oldestDueCommitted = Math.min(oldestDueCommitted, anchor);
				} else {
					scheduled++;
				}
				break;
			default:
				break;
		}
	}

	const hasActivity = lastActivity !== Number.NEGATIVE_INFINITY;
	const lastActivityAt = hasActivity ? new Date(lastActivity).toISOString() : null;
	const lastActivityMsAgo = hasActivity ? now - lastActivity : null;
	const oldestQueued =
		oldestDueCommitted === Number.POSITIVE_INFINITY ? null : now - oldestDueCommitted;

	const dueBacklog = queued;
	// "Silent" = no heartbeat, or the last one is older than the dead threshold.
	const silent = lastActivityMsAgo == null || lastActivityMsAgo >= T_DEAD_MS;

	let state: WorkerHealthState;
	if (dueBacklog > 0 && silent) {
		state = 'dead';
	} else if (
		(oldestQueued != null && oldestQueued >= T_EXPECT_MS && !silent) ||
		failuresThisHour >= DEGRADED_FAILURES
	) {
		state = 'degraded';
	} else if (silent) {
		// Nothing due AND no (or stale) heartbeat: the worker may well be fine,
		// but claiming green "Alive" beside "last activity 30h ago" reads as a
		// contradiction. Idle is the honest verdict — no signal, nothing owed.
		state = 'idle';
	} else {
		state = 'alive';
	}

	return {
		state,
		sending,
		queued,
		failed,
		failuresThisHour,
		scheduled,
		lastActivityAt,
		lastActivityMsAgo,
		oldestQueuedMsAgo: oldestQueued,
	};
}

export type DeliveryChipState = 'scheduled' | 'queued' | 'sending' | 'sent' | 'failed' | 'stalled';

/**
 * Per-row delivery chip state from the worker-authored `dbStatus` + worker
 * health. The queued↔stalled split is the only place liveness feeds a single
 * row: a committed row is "queued" while the worker is alive/degraded, and
 * "stalled" once it is dead. Returns null for pre-commit rows
 * (awaiting/edited/rejected) — they carry no delivery chip.
 */
export function deliveryChipState(
	delivery: DraftDelivery | undefined,
	healthState: WorkerHealthState | undefined,
	now: number = Date.now()
): DeliveryChipState | null {
	if (!delivery) return null;
	switch (delivery.dbStatus) {
		case 'sending':
			return 'sending';
		case 'sent':
			return 'sent';
		case 'failed':
			return 'failed';
		case 'committed': {
			const sched = delivery.scheduledAt ? Date.parse(delivery.scheduledAt) : Number.NaN;
			if (!Number.isNaN(sched) && sched > now) return 'scheduled';
			return healthState === 'dead' ? 'stalled' : 'queued';
		}
		default:
			return null;
	}
}
