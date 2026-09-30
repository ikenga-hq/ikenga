// Store / Installed-tab update approvals (WP-41-F1 for the Ngwa surfaces).
//
// An update whose new version asks for new permissions is held back by
// `useStoreInstall().update` / `updateAll` (`NeedsApprovalError`) — nothing is
// installed. This hook parks those holds and mounts the updater's own
// `TrustReviewModal` in controlled mode, exactly as `PkgUpdatePanel` does:
//   Approve → re-run the update with `{ approved: true }`, which skips the
//             pre-install capability diff (it would only park it again);
//   Reject  → drop it; nothing was installed, so there is nothing to undo.
// Mounted once per route: the Store route, and `useNgwaItemActions`' dialog
// (Installed tab, item detail).

import { useCallback, useMemo, useRef, useState, type ReactNode } from 'react';
import { TrustReviewModal } from '@/components/pkg/trust-review-modal';
import type { NgwaStoreEntry } from '@/lib/ngwa/enrichment';
import type { PendingUpdateApproval, StoreUpdateOptions } from '@/lib/ngwa/use-store-install';

export interface UseUpdateApprovalsOptions {
	/** The Store update (`useStoreInstall().update`). */
	update: (entry: NgwaStoreEntry, opts?: StoreUpdateOptions) => Promise<void>;
	/** An approved update installed. */
	onApproved?: (entry: NgwaStoreEntry) => void;
	/** The user rejected it; it stays on its current version. */
	onRejected?: (entry: NgwaStoreEntry) => void;
}

export interface UpdateApprovals {
	/** Updates waiting on the user's approval (they survive closing the modal). */
	pending: PendingUpdateApproval[];
	/** Park these holds and open the review. */
	request: (approvals: PendingUpdateApproval[]) => void;
	/** Reopen the review of whatever is pending. */
	review: () => void;
	/** The modal — render once. */
	element: ReactNode;
}

export function useUpdateApprovals({
	update,
	onApproved,
	onRejected,
}: UseUpdateApprovalsOptions): UpdateApprovals {
	const [pending, setPending] = useState<PendingUpdateApproval[]>([]);
	const [open, setOpen] = useState(false);
	// The modal's callbacks read the latest list without re-binding.
	const pendingRef = useRef(pending);
	pendingRef.current = pending;

	const request = useCallback((approvals: PendingUpdateApproval[]) => {
		if (approvals.length === 0) return;
		setPending((prev) => {
			// A re-requested pkg replaces its older review (a newer version).
			const ids = new Set(approvals.map((a) => a.review.pkg_id));
			return [...prev.filter((p) => !ids.has(p.review.pkg_id)), ...approvals];
		});
		setOpen(true);
	}, []);

	const review = useCallback(() => setOpen(true), []);

	const drop = useCallback((pkgId: string) => {
		// The modal closes itself once nothing is pending (`open` below).
		setPending((prev) => prev.filter((p) => p.review.pkg_id !== pkgId));
	}, []);

	const find = useCallback((pkgId: string) => {
		const hit = pendingRef.current.find((p) => p.review.pkg_id === pkgId);
		if (!hit) throw new Error(`${pkgId} is no longer waiting for approval`);
		return hit;
	}, []);

	const approve = useCallback(
		async (pkgId: string) => {
			const { entry } = find(pkgId);
			// A rejection surfaces in the modal's error line; the row stays.
			await update(entry, { approved: true });
			drop(pkgId);
			onApproved?.(entry);
		},
		[find, update, drop, onApproved]
	);

	const reject = useCallback(
		async (pkgId: string) => {
			const { entry } = find(pkgId);
			drop(pkgId);
			onRejected?.(entry);
		},
		[find, drop, onRejected]
	);

	const reviews = useMemo(() => pending.map((p) => p.review), [pending]);

	const element = (
		<TrustReviewModal
			open={open && pending.length > 0}
			onOpenChange={(v) => setOpen(v)}
			initialReviews={reviews}
			onApprove={approve}
			onReject={reject}
		/>
	);

	return { pending, request, review, element };
}
