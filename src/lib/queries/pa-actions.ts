import { queryOptions } from '@tanstack/react-query';
import { queryKeys } from '@/lib/query-keys';
import { paActionsList } from '@/lib/tauri-cmd';

/** TanStack Query options for the gate's active draft set (or a status filter).
 *  The Outbox queue itself lives in the com.ikenga.outbound pkg (plans/outbox-redesign
 *  D-01), which reads through `host.paActions.list`; the shell keeps this for its entry
 *  points only — the status-bar / rail pending count and the Explorer's Outbox entry. */
export function paActionsListQueryOptions(status?: string) {
	return queryOptions({
		queryKey: queryKeys.paActions.list(status),
		queryFn: () => paActionsList(status),
		staleTime: 10_000,
	});
}

/** The text to show on a draft for a failed approve / reject / edit. Share
 *  refusals arrive as `<code>: <sentence>`; show the sentence. */
export function draftActionErrorMessage(err: unknown): string {
	const raw = err instanceof Error ? err.message : typeof err === 'string' ? err : '';
	// The browser's own words for "no answer came back" mean nothing to a reviewer, and they do
	// not say whether the request landed: the reply may be what was lost.
	if (
		/^(Failed to fetch|NetworkError when attempting to fetch resource\.?|Load failed|Network request failed)$/i.test(
			raw.trim()
		)
	) {
		return "Couldn't reach the box (network error). Check your connection and try again. If you are unsure whether it went through, reload the list first.";
	}
	const http = /^HTTP RPC error: (\d{3})\b/.exec(raw);
	if (http) {
		return `The box did not answer properly (HTTP ${http[1]}). Try again in a moment; reload the list first if you are unsure whether it went through.`;
	}
	const m =
		/^(?:approval_unavailable|approval_refused|draft_changed|forbidden|invalid_request): (.+)$/s.exec(
			raw
		);
	const text = (m ? m[1] : raw).trim();
	return text || 'The action failed. Nothing was changed.';
}
