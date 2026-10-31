// Whether the Explorer shows its shell-core "Outbox" entry.
//
// The entry appears on its own when an approvals backend is active, and goes
// away when it is not:
//   * Browser tab against a box: the daemon's `outbox_approvals_status` arm —
//     the sender add-on's approvals socket accepts a connection and is served
//     by an allowed uid (the broker's own rule). Cached ~60 s, so installing or
//     stopping the add-on shows up without a reload.
//   * Desktop: the local approve-gate is in use — the `com.ikenga.outbound`
//     pkg is installed and enabled, or `pa_action_drafts` has active rows
//     (the same set the status bar counts).
//   * Inside a share: only an Operator can use Outbox (an owner is never "in"
//     a share), so Reviewer / Guest never see it.

import { useQuery } from '@tanstack/react-query';
import { paActionsListQueryOptions } from '@/lib/queries/pa-actions';
import { pkgKernelStatus } from '@/lib/tauri-cmd';
import { invoke, isBrowserSession } from '@/lib/transport';
import { useCurrentShare } from '@/shell/people/use-shared-projects';

export const OUTBOUND_PKG_ID = 'com.ikenga.outbound';
export const OUTBOX_ACTIVE_STALE_MS = 60_000;

export const outboxApprovalsStatusQueryKey = ['outbox', 'approvals-status'] as const;

export async function fetchOutboxApprovalsActive(): Promise<boolean> {
	try {
		const r = await invoke<{ active?: unknown } | null>('outbox_approvals_status');
		return r?.active === true;
	} catch {
		// A server without the arm, or a refusal: no backend to show.
		return false;
	}
}

export function useOutboxApprovalsActive(): boolean {
	const share = useCurrentShare();
	const browser = isBrowserSession();

	const box = useQuery({
		queryKey: outboxApprovalsStatusQueryKey,
		queryFn: fetchOutboxApprovalsActive,
		enabled: browser,
		staleTime: OUTBOX_ACTIVE_STALE_MS,
		refetchInterval: OUTBOX_ACTIVE_STALE_MS,
		retry: false,
	});
	const kernel = useQuery({
		queryKey: ['pkg', 'kernel-status'],
		queryFn: pkgKernelStatus,
		enabled: !browser,
		refetchOnWindowFocus: false,
	});
	const drafts = useQuery({
		...paActionsListQueryOptions(),
		enabled: !browser,
	});

	if (share && share.role !== 'operator') return false;
	if (browser) return box.data === true;
	const outboundInstalled = (kernel.data?.installed ?? []).some(
		(p) => p.id === OUTBOUND_PKG_ID && p.enabled !== false
	);
	return outboundInstalled || (drafts.data?.length ?? 0) > 0;
}
