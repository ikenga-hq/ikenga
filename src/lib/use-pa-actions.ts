import { useQueryClient } from '@tanstack/react-query';
import { useEffect } from 'react';
import { usePaneStore } from '@/lib/panes/pane-store';
import { queryKeys } from '@/lib/query-keys';
import { onPaActionPaused } from '@/lib/tauri-cmd';
import { isOutboundPkgPath, OUTBOX_APPROVALS_ROUTE } from '@/shell/outbox-deeplink';

/**
 * Shell-level listener for the approve-gate seam (WP-7). When an approve-aware
 * action pauses a batch (`pa-action-paused`), refresh the shell's pending count
 * and open the Outbox queue — the com.ikenga.outbound pkg's inbox, reached through
 * the `/outbox/approvals` deep-link (plans/outbox-redesign D-01) — in the focused
 * pane. A pane already showing the pkg is left alone: the operator may be
 * mid-edit there, and the pkg's own list picks the new drafts up.
 * Mount once at workspace level. Scope: plans/atelier/10-approve-gate-seam.md.
 */
export function usePaActionsListener(): void {
	const qc = useQueryClient();
	useEffect(() => {
		let unlisten: (() => void) | undefined;
		void (async () => {
			unlisten = await onPaActionPaused(() => {
				qc.invalidateQueries({ queryKey: queryKeys.paActions.all });
				const panes = usePaneStore.getState();
				const view = panes.focusedView();
				if (view?.kind === 'route' && isOutboundPkgPath(view.path)) return;
				panes.navigateFocused(OUTBOX_APPROVALS_ROUTE);
			});
		})();
		return () => unlisten?.();
	}, [qc]);
}
