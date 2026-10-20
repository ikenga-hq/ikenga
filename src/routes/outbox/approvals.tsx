// The approve-gate route (WP-6) — mounts the run-then-pause draft-review panel
// against live pa_action_drafts. Opened by `usePaActionsListener` on
// `pa-action-paused`. Scope: plans/atelier/10-approve-gate-seam.md.

import { useQuery, useQueryClient } from '@tanstack/react-query';
import { createFileRoute } from '@tanstack/react-router';
import { useCallback, useMemo, useState } from 'react';
import {
	deriveWorkerHealth,
	draftActionErrorMessage,
	paActionsListQueryOptions,
	pausedDraftFromRow,
} from '@/lib/queries/pa-actions';
import { queryKeys } from '@/lib/query-keys';
import {
	type PaDraftSeen,
	paActionsCommit,
	paActionsReject,
	paActionsRetry,
	paActionsUpdate,
} from '@/lib/tauri-cmd';
import { ApproveGatePanel } from '@/shell/atelier/surfaces/approve-gate-panel';
import { useCurrentShare } from '@/shell/people/use-shared-projects';

/** Shown above the queue inside a share (box plan WP-5). */
const SHARE_NOTE =
	'Approving here records you as the approver in the box\u2019s approval ledger, and the box sends the mail (about a minute after the 10 s undo). Your edits are saved to the box first; save before you approve.';

function ApprovalsPage() {
	const qc = useQueryClient();
	// Inside a share the drafts are the owner's, on the box. Outside one nothing below changes.
	const share = useCurrentShare();
	const inShare = share !== null;
	// A failed approve / reject / edit, by draft id: shown on the draft (the panel removes a
	// row as soon as the 10 s undo ends, and brings it back when its id lands here).
	const [errors, setErrors] = useState<Record<string, string>>({});
	const setError = useCallback((id: string, err: unknown) => {
		setErrors((prev) => ({ ...prev, [id]: draftActionErrorMessage(err) }));
	}, []);
	const clearError = useCallback((id: string) => {
		setErrors((prev) => {
			if (!(id in prev)) return prev;
			const { [id]: _gone, ...rest } = prev;
			return rest;
		});
	}, []);
	// WP-11: poll while the surface is open so the client-side worker-liveness
	// derivation stays live (the strip's only clock is this query's freshness).
	const { data, error: listError } = useQuery({
		...paActionsListQueryOptions(),
		refetchInterval: 15_000,
	});

	const drafts = useMemo(
		() =>
			(data ?? []).map(pausedDraftFromRow).filter((d): d is NonNullable<typeof d> => d !== null),
		[data]
	);

	// WP-11: worker health is a pure client-side derivation over the same rows —
	// no new IPC, no daemon heartbeat (design's honest-dead default).
	const health = useMemo(() => deriveWorkerHealth(data ?? []), [data]);

	const invalidate = () => qc.invalidateQueries({ queryKey: queryKeys.paActions.all });

	// The panel fires onApprove only AFTER its own 10s undo window elapses; commit
	// then flips the row to `committed` and emits pa-action-committed for the worker.
	// Inside a share the commit carries the row the reviewer was shown (`seen`): the broker
	// binds the ledger approval to it, and refuses if the box's draft is different.
	const onApprove = (id: string, seen?: PaDraftSeen) => {
		clearError(id);
		if (!inShare) {
			void paActionsCommit(id).then(invalidate);
			return;
		}
		paActionsCommit(id, seen).then(invalidate, (e) => {
			setError(id, e);
			// A "changed since you reviewed it" refusal means the list is stale too.
			void invalidate();
		});
	};
	const onReject = (id: string) => {
		if (!inShare) {
			void paActionsReject(id).then(invalidate);
			return;
		}
		paActionsReject(id).then(invalidate, (e) => setError(id, e));
	};
	// Re-queue a failed draft: failed → committed + event-wake the worker (WP-12 / G-09).
	// Not offered in a share: a retry re-queues a send whose approval is spent.
	const onRetry = (id: string) => void paActionsRetry(id).then(invalidate);
	// Fire-and-forget persist — the panel already reflects the edit locally. In a share the
	// list is refetched after the save, so the row the next approval is bound to includes it.
	const onEdit = (id: string, patch: { subject?: string; body?: string }) => {
		if (!inShare) {
			void paActionsUpdate(id, patch);
			return;
		}
		paActionsUpdate(id, patch).then(invalidate, (e) => setError(id, e));
	};

	// Inside a share only an Operator may review the owner's drafts: a Reviewer or Guest is
	// refused by the broker. Say so, rather than showing an empty queue that reads as "nothing pending".
	if (inShare && listError) {
		return (
			<div className="h-full bg-background p-6" role="alert">
				<p className="text-sm font-medium">Drafts are not available in this share</p>
				<p className="mt-1 text-sm text-muted-foreground">{draftActionErrorMessage(listError)}</p>
			</div>
		);
	}

	return (
		<div className="h-full bg-background">
			<ApproveGatePanel
				drafts={drafts}
				health={health}
				onApprove={onApprove}
				onReject={onReject}
				onEdit={onEdit}
				onRetry={inShare ? undefined : onRetry}
				shareNote={inShare ? SHARE_NOTE : undefined}
				draftErrors={inShare ? errors : undefined}
			/>
		</div>
	);
}

export const Route = createFileRoute('/outbox/approvals')({
	component: ApprovalsPage,
});
