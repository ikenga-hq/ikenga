// WP-P9 — the server-update banner (browser sessions, admins only).
//
// One entry in the single banner slot (`slots/banner-slot.tsx`), right after
// the desktop updater. It renders nothing unless `useServerUpdate()` has a
// view, which only a T1 admin or the T0 operator ever gets — a member, a
// paired device and the desktop app see nothing.
//
// States, first match wins:
//   reload     — the server now runs the version this tab's bundle predates
//   running    — root is applying an update
//   pending    — a request waits for root to claim it
//   outcome    — the last run rolled back / failed / was refused (recent,
//                dismissable per request)
//   available  — a newer release; [Review update] opens Settings › About,
//                [Later] snoozes that version for 24 h
//
// The banner never applies anything: the confirm (with the open-terminal
// count) lives in the Settings › About panel.

import { AlertTriangle, CheckCircle2, Download, Loader2, RefreshCw, XCircle } from 'lucide-react';
import { useReducer } from 'react';
import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import { gotoRoute } from '@/lib/actions/runner/open';
import { needsReload, useServerUpdate } from '@/lib/queries/server-update';
import type { ServerUpdateRun } from '@/lib/transport/server-update';

const SNOOZE_KEY = 'ikenga.server-update.snooze';
const DISMISS_KEY = 'ikenga.server-update.dismissed-run';
const SNOOZE_MS = 24 * 60 * 60 * 1000;
/** An outcome older than this is history, not news. */
const OUTCOME_FRESH_MS = 24 * 60 * 60 * 1000;

export const SERVER_UPDATE_ROUTE = '/settings/about';

export function plural(n: number, word: string): string {
	return `${n} ${word}${n === 1 ? '' : 's'}`;
}

/** "Updating restarts it and ends 3 open terminals." */
export function terminalsCopy(n: number, partial: boolean): string {
	if (n === 0 && !partial) return 'No terminals are open.';
	const count = partial ? `at least ${plural(n, 'open terminal')}` : plural(n, 'open terminal');
	return `Updating restarts it and ends ${count}.`;
}

function readJson<T>(key: string): T | null {
	try {
		const raw = localStorage.getItem(key);
		return raw ? (JSON.parse(raw) as T) : null;
	} catch {
		return null;
	}
}

function writeJson(key: string, value: unknown): void {
	try {
		localStorage.setItem(key, JSON.stringify(value));
	} catch {
		// Private mode / blocked storage: the snooze just doesn't persist.
	}
}

export function isSnoozed(version: string): boolean {
	const s = readJson<{ version?: string; until?: number }>(SNOOZE_KEY);
	return !!s && s.version === version && typeof s.until === 'number' && s.until > Date.now();
}

function snooze(version: string): void {
	writeJson(SNOOZE_KEY, { version, until: Date.now() + SNOOZE_MS });
}

function isDismissed(run: ServerUpdateRun): boolean {
	const id = run.request_id ?? `${run.to}@${run.finished_at}`;
	return readJson<string>(DISMISS_KEY) === id;
}

function dismiss(run: ServerUpdateRun): void {
	writeJson(DISMISS_KEY, run.request_id ?? `${run.to}@${run.finished_at}`);
}

function isFresh(iso: string | null): boolean {
	if (!iso) return false;
	const t = Date.parse(iso);
	return Number.isFinite(t) && Date.now() - t < OUTCOME_FRESH_MS;
}

function ReviewLink() {
	return (
		<button
			type="button"
			onClick={() => gotoRoute(SERVER_UPDATE_ROUTE)}
			className="font-mono text-[11px] text-muted-foreground hover:text-foreground"
		>
			Details →
		</button>
	);
}

export function ServerUpdateBanner() {
	const { data: view } = useServerUpdate();
	const [, rerender] = useReducer((n: number) => n + 1, 0);
	if (!view) return null;
	const run = view.last_run;

	if (needsReload(view)) {
		return (
			<Banner
				data-state="server-update-reload"
				tone="success"
				icon={<CheckCircle2 />}
				actions={
					<Button size="sm" onClick={() => window.location.reload()}>
						Reload
					</Button>
				}
			>
				<span className="font-medium">The server was updated to Ikenga {view.current}.</span>
				<span className="text-muted-foreground"> Reload to use it.</span>
			</Banner>
		);
	}

	if (run?.state === 'running') {
		return (
			<Banner
				data-state="server-update-running"
				tone="info"
				icon={<Loader2 className="animate-spin" />}
				actions={<ReviewLink />}
			>
				<span className="font-medium">Updating the server to Ikenga {run.to ?? '…'}…</span>
				<span className="text-muted-foreground">
					{' '}
					It restarts on its own; this page reconnects.
				</span>
			</Banner>
		);
	}

	if (view.pending_request) {
		return (
			<Banner
				data-state="server-update-pending"
				tone="info"
				icon={<Loader2 className="animate-spin" />}
				actions={<ReviewLink />}
			>
				<span className="font-medium">
					Server update to Ikenga {view.pending_request.version ?? '…'} requested
				</span>
				<span className="text-muted-foreground"> — waiting for the server to start it.</span>
			</Banner>
		);
	}

	if (run && isFresh(run.finished_at ?? run.started_at) && !isDismissed(run)) {
		const onDismiss = () => {
			dismiss(run);
			rerender();
		};
		if (run.state === 'rolled_back') {
			return (
				<Banner
					data-state="server-update-rolled-back"
					tone="warning"
					icon={<AlertTriangle />}
					actions={<ReviewLink />}
					onDismiss={onDismiss}
					dismissLabel="Dismiss"
				>
					<span className="font-medium">
						The update to Ikenga {run.to ?? '?'} failed its health check
					</span>
					<span className="text-muted-foreground">
						{' '}
						and was rolled back to {run.from ?? 'the previous version'}.
					</span>
				</Banner>
			);
		}
		if (run.state === 'failed' || run.state === 'interrupted') {
			return (
				<Banner
					data-state="server-update-failed"
					tone="danger"
					icon={<XCircle />}
					actions={<ReviewLink />}
					onDismiss={onDismiss}
					dismissLabel="Dismiss"
				>
					The server update to Ikenga {run.to ?? '?'} failed; the server may need attention (see
					Settings › About).
				</Banner>
			);
		}
		if (run.state === 'refused') {
			return (
				<Banner
					data-state="server-update-refused"
					tone="info"
					icon={<RefreshCw />}
					actions={<ReviewLink />}
					onDismiss={onDismiss}
					dismissLabel="Dismiss"
				>
					<span className="text-muted-foreground">
						The server declined the update request{run.message ? `: ${run.message}` : '.'}
					</span>
				</Banner>
			);
		}
	}

	const available = view.available;
	if (!available || available.blocked || isSnoozed(available.version)) return null;
	return (
		<Banner
			data-state="server-update-available"
			tone="info"
			icon={<Download />}
			onDismiss={() => {
				snooze(available.version);
				rerender();
			}}
			dismissLabel="Later"
			actions={
				<Button size="sm" onClick={() => gotoRoute(SERVER_UPDATE_ROUTE)}>
					Review update
				</Button>
			}
		>
			<span className="font-medium">Ikenga {available.version}</span>
			<span className="text-muted-foreground">
				{' '}
				is available for this server.{' '}
				{terminalsCopy(view.open_terminals, view.open_terminals_partial)}
			</span>
		</Banner>
	);
}
