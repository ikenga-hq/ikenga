// Settings › About › Server updates (WP-P9 steps 2–3).
//
// Shown only when `useServerUpdate()` has a view: a browser tab of a T1 admin
// or the T0 operator, on a server whose root units are installed. Founder
// decision 2026-10-06: admins may update; checking is notify-only and nothing
// is ever applied without this confirm.
//
// [Update now] opens a confirm that restates the version and the open
// terminals the restart will end; Confirm sends that count as
// `acknowledged_open_terminals`. If more are open by then, the server answers
// `terminals_open` with the new count and the dialog re-opens with it. Root
// does the update (scripts/server/provision.sh apply-request); this panel
// polls its status file through the server and shows the outcome.

import { useQueryClient } from '@tanstack/react-query';
import { AlertTriangle, Download, ExternalLink, Loader2, RefreshCw } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { queryKeys } from '@/lib/query-keys';
import { needsReload, useServerUpdate } from '@/lib/queries/server-update';
import {
	type ApplyServerUpdateResult,
	applyServerUpdate,
	type ServerUpdateBlockedReason,
	type ServerUpdateRun,
	type ServerUpdateView,
} from '@/lib/transport/server-update';
import { plural, terminalsCopy } from '@/shell/updater/server-update-banner';

import { SettingGroup } from './setting-group';
import { formatRelative } from './update-status';

export const BLOCKED_REASON_COPY: Record<ServerUpdateBlockedReason, string> = {
	unsupported: 'Server updates are not managed on this host.',
	none_available: 'The server is up to date.',
	blocked_min_upgrade: 'This release needs a newer base version first; update over SSH.',
	running: 'An update is running.',
	pending: 'An update request is waiting for the server to start it.',
	cooldown: 'This version failed less than an hour ago; try again later.',
};

const APPLY_ERROR_COPY: Record<string, string> = {
	pending: BLOCKED_REASON_COPY.pending,
	update_running: BLOCKED_REASON_COPY.running,
	cooldown: BLOCKED_REASON_COPY.cooldown,
	blocked: BLOCKED_REASON_COPY.blocked_min_upgrade,
	not_advertised: 'That version is no longer the available update. Refresh and try again.',
	throttled: 'An update was requested moments ago.',
	forbidden: 'Only an administrator can update the server.',
	unsupported: BLOCKED_REASON_COPY.unsupported,
};

const RUN_STATE_COPY: Record<ServerUpdateRun['state'], string> = {
	running: 'Running',
	succeeded: 'Updated',
	noop: 'Already up to date',
	rolled_back: 'Rolled back',
	failed: 'Failed',
	refused: 'Refused',
	interrupted: 'Interrupted',
};

function isoRelative(iso: string | null): string {
	if (!iso) return 'never';
	const t = Date.parse(iso);
	return Number.isFinite(t) ? formatRelative(t) : iso;
}

/** The panel, wired to the query. Renders nothing without a view. */
export function ServerUpdatePanel() {
	const { data: view } = useServerUpdate();
	const queryClient = useQueryClient();
	if (!view) return null;
	return (
		<ServerUpdateCard
			view={view}
			reloadNeeded={needsReload(view)}
			apply={applyServerUpdate}
			onChanged={() => void queryClient.invalidateQueries({ queryKey: queryKeys.serverUpdate.all })}
		/>
	);
}

export function ServerUpdateCard({
	view,
	reloadNeeded,
	apply,
	onChanged,
}: {
	view: ServerUpdateView;
	reloadNeeded: boolean;
	apply: (input: {
		version: string;
		acknowledgedOpenTerminals: number;
	}) => Promise<ApplyServerUpdateResult>;
	onChanged: () => void;
}) {
	/** The terminal count the open confirm shows (and will acknowledge). */
	const [confirmCount, setConfirmCount] = useState<number | null>(null);
	const [sending, setSending] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const available = view.available;
	const run = view.last_run;
	const running = run?.state === 'running' || view.pending_request !== null;
	const blockedReason = view.apply_blocked_reason;

	async function confirm() {
		if (!available || confirmCount === null) return;
		setSending(true);
		setError(null);
		const result = await apply({
			version: available.version,
			acknowledgedOpenTerminals: confirmCount,
		});
		setSending(false);
		if (result.ok) {
			setConfirmCount(null);
			onChanged();
			return;
		}
		if (result.code === 'terminals_open' && typeof result.openTerminals === 'number') {
			// More terminals opened since the dialog was shown: confirm again.
			setConfirmCount(result.openTerminals);
			setError(`More terminals are open now (${result.openTerminals}). Confirm again to continue.`);
			return;
		}
		setConfirmCount(null);
		setError(APPLY_ERROR_COPY[result.code] ?? result.message);
		onChanged();
	}

	return (
		<div id="server-update" data-testid="server-update-panel">
			<SettingGroup title="Server updates">
				<div className="space-y-3 px-4 py-3.5 text-sm">
					<div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
						<span className="text-muted-foreground">This server runs</span>
						<span className="rounded-sm border border-border bg-background px-1.5 py-0.5 font-mono text-[11px]">
							v{view.current}
						</span>
						{available && (
							<>
								<span className="text-muted-foreground">→</span>
								<span
									className="rounded-sm border border-border bg-background px-1.5 py-0.5 font-mono text-[11px]"
									data-testid="server-update-available"
								>
									v{available.version} available
								</span>
								{available.notes_url && (
									<a
										href={available.notes_url}
										target="_blank"
										rel="noopener noreferrer"
										className="inline-flex items-center gap-1 font-mono text-[11px] text-muted-foreground hover:text-foreground"
									>
										Release notes <ExternalLink className="size-3" />
									</a>
								)}
							</>
						)}
					</div>

					<div className="font-mono text-[11px] text-muted-foreground/80">
						Last checked {isoRelative(view.checked_at)}
						{view.check_error && (
							<span className="text-[var(--danger)]"> · check failed: {view.check_error}</span>
						)}
					</div>

					{available?.blocked && (
						<p
							className="flex items-start gap-2 text-muted-foreground"
							data-testid="server-update-blocked"
						>
							<AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
							{available.min_upgrade_from
								? `This release requires v${available.min_upgrade_from} first; update over SSH.`
								: (available.blocked_reason ?? BLOCKED_REASON_COPY.blocked_min_upgrade)}
						</p>
					)}

					{available && !available.blocked && (
						<p className="text-muted-foreground" data-testid="server-update-terminals">
							{terminalsCopy(view.open_terminals, view.open_terminals_partial)}
							{view.open_terminals_partial && ' (count may be incomplete)'}
						</p>
					)}

					{reloadNeeded && (
						<div className="flex items-center gap-3 rounded-sm border border-[var(--border)] px-3 py-2">
							<span className="flex-1">
								The server was updated to v{view.current}. Reload to use it.
							</span>
							<Button size="sm" onClick={() => window.location.reload()}>
								Reload
							</Button>
						</div>
					)}

					{running && (
						<div
							className="flex items-center gap-2 text-muted-foreground"
							data-testid="server-update-running"
						>
							<Loader2 className="size-3.5 animate-spin" />
							{run?.state === 'running'
								? `Updating to v${run.to ?? '…'} — started ${isoRelative(run.started_at)}. The server restarts on its own.`
								: `Update to v${view.pending_request?.version ?? '…'} requested — waiting for the server to start it.`}
						</div>
					)}

					{error && (
						<div
							role="alert"
							className="rounded-sm border border-[var(--danger)] px-3 py-2 text-[var(--danger)]"
						>
							{error}
						</div>
					)}

					{available && (
						<div className="flex items-center justify-end gap-3">
							{!view.can_apply && blockedReason && (
								<span className="text-xs text-muted-foreground" data-testid="server-update-reason">
									{BLOCKED_REASON_COPY[blockedReason]}
								</span>
							)}
							<span
								title={
									!view.can_apply && blockedReason ? BLOCKED_REASON_COPY[blockedReason] : undefined
								}
							>
								<Button
									size="sm"
									disabled={!view.can_apply || sending}
									onClick={() => {
										setError(null);
										setConfirmCount(view.open_terminals);
									}}
								>
									<Download className="mr-1.5 size-3.5" />
									Update now
								</Button>
							</span>
						</div>
					)}
				</div>

				{run && <LastRun run={run} />}
			</SettingGroup>

			<Dialog
				open={confirmCount !== null}
				onOpenChange={(open) => {
					if (!open && !sending) setConfirmCount(null);
				}}
			>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>Update the server to Ikenga {available?.version}?</DialogTitle>
						<DialogDescription>
							The server restarts to finish the update.{' '}
							{confirmCount === 0
								? 'No terminals are open.'
								: `${plural(confirmCount ?? 0, 'open terminal')} will end${view.open_terminals_partial ? ' (the count may be incomplete)' : ''}.`}{' '}
							If the new version fails its health check, the server rolls back on its own.
						</DialogDescription>
					</DialogHeader>
					{error && confirmCount !== null && (
						<p role="alert" className="text-sm text-[var(--danger)]">
							{error}
						</p>
					)}
					<DialogFooter>
						<Button variant="ghost" disabled={sending} onClick={() => setConfirmCount(null)}>
							Cancel
						</Button>
						<Button disabled={sending} onClick={() => void confirm()}>
							{sending ? (
								<Loader2 className="mr-1.5 size-3.5 animate-spin" />
							) : (
								<RefreshCw className="mr-1.5 size-3.5" />
							)}
							Update and restart
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</div>
	);
}

function LastRun({ run }: { run: ServerUpdateRun }) {
	return (
		<div className="space-y-2 px-4 py-3 text-sm" data-testid="server-update-last-run">
			<div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
				<span className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
					Last update
				</span>
				<span className="font-medium" data-testid="server-update-run-state">
					{RUN_STATE_COPY[run.state] ?? run.state}
				</span>
				{run.from && run.to && (
					<span className="font-mono text-[11px] text-muted-foreground">
						v{run.from} → v{run.to}
					</span>
				)}
				{run.rolled_back && (
					<span className="rounded-sm border border-border bg-background px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
						rolled back
					</span>
				)}
			</div>
			<div className="font-mono text-[11px] text-muted-foreground/80">
				{run.requested_by && <>by {run.requested_by} · </>}
				started {isoRelative(run.started_at)}
				{run.finished_at && <> · finished {isoRelative(run.finished_at)}</>}
			</div>
			{run.message && <p className="text-muted-foreground">{run.message}</p>}
			{run.log_tail.length > 0 && (
				<details>
					<summary className="cursor-pointer font-mono text-[11px] text-muted-foreground">
						Log ({run.log_tail.length} lines)
					</summary>
					<pre
						className="mt-2 max-h-64 overflow-auto rounded-md border border-border bg-background p-3 font-mono text-[11px] leading-relaxed"
						data-testid="server-update-log"
					>
						{run.log_tail.join('\n')}
					</pre>
				</details>
			)}
		</div>
	);
}
