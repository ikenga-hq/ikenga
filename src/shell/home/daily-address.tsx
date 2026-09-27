// WP-39 — D-04 `daily-address`: the day-start summary on the Project
// dashboard (`src/routes/project/dashboard.tsx`, mounted above `<Home/>` as
// the first widget row).
//
// designs/onboarding.html?state=daily-address (`renderDash()`) is the source
// mockup; `plans/shell-ux-rearchitecture/drafts/design-spec-D-03-07.md` §D-04
// names the four groups: "Since you were last here" (runs), "Waiting on you"
// (permissions), "Updates", "Todos" (4). Real data only — no source here is
// fabricated:
//
//   tile          | source                                    | real API
//   --------------|-------------------------------------------|---------------------------------
//   runs          | cached Chi runs whose cwd is under the     | chiList() (chi.rs `chi_list`),
//                 | active project's root                      | filtered client-side
//   permissions   | WP-40 notifications, kind=permission,      | notificationsListQueryOptions()
//                 | still unresolved (`resolvedAt == null`)    | + isNotificationResolved()
//   updates       | WP-40 notifications, kind=update           | notificationsListQueryOptions()
//                 | + live updater state                       | useUpdater() / usePkgsDerived()
//   todos         | native todos table, active project scope   | listTodos() (@/lib/iyke/memory)
//
// Where a tile has nothing to show, it renders WP-43's <EmptyState> with
// exactly one next action (D-07 rule) instead of an empty card.
//
// Shown once per day, dismissible, re-openable from the title row
// (`title-row.tsx`'s `DailyAddressReopenButton`, rendered only while
// dismissed for today). Persistence: `dailyAddressDismissedOn` on the plain
// Zustand-persisted half of `shell-store.ts` — see the field's doc comment
// there for why the per-day dismissal uses the shell-store. Turning the
// address off altogether is a real setting: `workspace.dailyAddress` in
// settings.json (Settings › Workspace, `src/lib/settings/daily-address.ts`).

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import {
	Clock,
	Inbox,
	ListChecks,
	RefreshCw,
	Settings as SettingsIcon,
	ShieldAlert,
	Sparkles,
	X,
} from 'lucide-react';
import { type ReactNode, useState } from 'react';
import { EmptyState, ErrorState, LoadingState } from '@/components/states';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import { cn } from '@/components/ui/utils';
import { iykeFetch } from '@/lib/iyke/client';
import { completeTodo, listTodos, type Todo } from '@/lib/iyke/memory';
import { asKnownNotificationAction } from '@/lib/notifications/action-kind';
import { usePaneStore } from '@/lib/panes/pane-store';
import { usePkgsDerived } from '@/lib/pkgs/use-derived';
import { useUpdatePkgs } from '@/lib/pkgs/use-update-pkgs';
import { queryKeys } from '@/lib/query-keys';
import {
	invalidateNotifications,
	isNotificationResolved,
	notificationDecision,
	notificationsListQueryOptions,
	useMarkAllNotificationsRead,
} from '@/lib/queries/notifications';
import { readSettingsFile } from '@/lib/settings/client';
import { isDailyAddressEnabled } from '@/lib/settings/daily-address';
import { useShellStore } from '@/lib/shell/shell-store';
import { chiList, type ChiCacheRow, type NotificationRow } from '@/lib/tauri-cmd';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import { useUpdater } from '@/lib/updater/use-updater';
import { runConsecrationAgain } from '@/shell/onboarding/run-again';

/** Local (not UTC) calendar date, `YYYY-MM-DD`. Deliberately not
 *  `toISOString()` (UTC) — "today" must track the user's own midnight. */
export function todayLocalDate(now: Date = new Date()): string {
	const y = now.getFullYear();
	const m = String(now.getMonth() + 1).padStart(2, '0');
	const d = String(now.getDate()).padStart(2, '0');
	return `${y}-${m}-${d}`;
}

const RECENT_WINDOW_MS = 24 * 60 * 60 * 1000;

function relativeTime(iso: string | null | undefined): string {
	if (!iso) return '';
	const t = new Date(iso).getTime();
	if (Number.isNaN(t)) return '';
	const diffMin = Math.round((Date.now() - t) / 60_000);
	if (diffMin < 1) return 'just now';
	if (diffMin < 60) return `${diffMin}m ago`;
	const diffHr = Math.round(diffMin / 60);
	if (diffHr < 24) return `${diffHr}h ago`;
	return new Date(t).toLocaleDateString(undefined, { weekday: 'short' });
}

// ─── shared tile chrome ─────────────────────────────────────────────────────

function Tile({
	title,
	count,
	children,
	footer,
}: {
	title: string;
	count?: string;
	children: ReactNode;
	footer?: ReactNode;
}) {
	return (
		<Card className="flex min-h-[168px] flex-col gap-0 py-0">
			<CardHeader className="flex-row items-center justify-between gap-2 border-b border-border px-3 py-2.5">
				<CardTitle className="text-xs font-medium text-muted-foreground">{title}</CardTitle>
				{count && (
					<span className="font-mono text-[10px] tabular-nums text-muted-foreground/80">
						{count}
					</span>
				)}
			</CardHeader>
			<CardContent className="flex-1 px-0 py-1">{children}</CardContent>
			{footer && (
				<CardFooter className="flex items-center gap-2 border-t border-border px-3 py-2 text-xs text-muted-foreground">
					{footer}
				</CardFooter>
			)}
		</Card>
	);
}

/** Plain informational row — deliberately not `ListRow` (which is a
 *  `role="button" tabIndex={0}` shell): most rows here have nothing to
 *  activate, and a focusable element with no `onActivate` is a dead tab
 *  stop. Real actions render as a real `<Button>` in `actions`. */
function Row({
	name,
	subtitle,
	timestamp,
	actions,
}: {
	name: ReactNode;
	subtitle?: ReactNode;
	timestamp?: ReactNode;
	actions?: ReactNode;
}) {
	return (
		<div className="flex items-center gap-2 px-3 py-1.5 text-xs">
			{subtitle != null ? (
				<div className="flex min-w-0 flex-1 flex-col">
					<span className="truncate font-medium text-foreground">{name}</span>
					<span className="truncate text-[10px] text-muted-foreground/80">{subtitle}</span>
				</div>
			) : (
				<span className="min-w-0 flex-1 truncate text-foreground">{name}</span>
			)}
			{timestamp != null && (
				<span className="shrink-0 font-mono text-[10px] tabular-nums text-muted-foreground">
					{timestamp}
				</span>
			)}
			{actions}
		</div>
	);
}

// ─── since you were last here (runs) ────────────────────────────────────────

const RUN_STATUS_LABEL: Record<string, string> = {
	done: 'ok',
	failed: 'failed',
	cancelled: 'cancelled',
	running: 'running',
	queued: 'queued',
};

function isRecentOrActive(row: ChiCacheRow): boolean {
	if (row.status === 'running' || row.status === 'queued') return true;
	const stamp = row.ended_at ?? row.last_seen_at ?? row.started_at;
	if (!stamp) return false;
	const t = new Date(stamp).getTime();
	return !Number.isNaN(t) && Date.now() - t <= RECENT_WINDOW_MS;
}

/** Forward slashes, no trailing separator — so `C:\a\b` and `/a/b/` compare. */
function normalizePath(p: string): string {
	const s = p.replace(/\\/g, '/');
	return s.length > 1 ? s.replace(/\/+$/, '') : s;
}

/** `cwd` is the project root or somewhere beneath it (not a sibling that
 *  merely shares the prefix: `/code/app2` is not under `/code/app`). */
export function isUnderProjectRoot(cwd: string | null | undefined, root: string): boolean {
	if (!cwd) return false;
	const c = normalizePath(cwd);
	const r = normalizePath(root);
	return c === r || c.startsWith(r.endsWith('/') ? r : `${r}/`);
}

/**
 * The runs the "Since you were last here" tile counts: recent or still
 * going, and — when a project is active — started in that project (its
 * `cwd` under the project root). Runs with no recorded `cwd` can't be
 * attributed and are left out of a project's address. No active project
 * root = no scope to apply.
 */
export function projectRecentRuns(
	rows: readonly ChiCacheRow[],
	projectRoot: string | null,
): ChiCacheRow[] {
	return rows
		.filter(isRecentOrActive)
		.filter((r) => projectRoot == null || isUnderProjectRoot(r.cwd, projectRoot));
}

/** Enough history that a busy other project doesn't push this one's day out
 *  of the window before the client-side project filter runs. */
const RUNS_FETCH_LIMIT = 200;

function useActiveProjectRoot(): string | null {
	return useShellStore(
		(s) => s.projects.find((p) => p.id === s.activeProjectId)?.root_path ?? null
	);
}

function useProjectRuns() {
	const projectRoot = useActiveProjectRoot();
	const query = useQuery({
		queryKey: queryKeys.dailyAddress.runs(),
		queryFn: () => chiList(null, RUNS_FETCH_LIMIT),
		staleTime: 30_000,
	});
	const runs = query.data ? projectRecentRuns(query.data, projectRoot) : undefined;
	return { ...query, runs };
}

function RunsTile() {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const { runs, isLoading, isError, refetch } = useProjectRuns();

	if (isLoading) {
		return <LoadingState data-state="daily-address-runs-loading" heading="Checking recent runs…" />;
	}
	if (isError) {
		return (
			<ErrorState
				data-state="daily-address-runs-error"
				heading="Couldn't load recent runs"
				action={{ label: 'Retry', onClick: () => void refetch() }}
			/>
		);
	}

	const all = runs ?? [];
	const recent = all.slice(0, 6);
	if (recent.length === 0) {
		return (
			<EmptyState
				data-state="daily-address-runs-empty"
				icon={Clock}
				heading="No runs since you were last here"
				action={{
					label: 'Open Automations',
					onClick: () => navigateFocused('/automations?view=runs'),
				}}
			/>
		);
	}

	return (
		<Tile
			title="Since you were last here"
			count={`${all.length} run${all.length === 1 ? '' : 's'}`}
			footer={
				<button
					type="button"
					className="underline-offset-2 hover:underline"
					onClick={() => navigateFocused('/automations?view=runs')}
				>
					Full history in Automations
				</button>
			}
		>
			{recent.map((r) => (
				<Row
					key={r.run_id}
					name={r.brief || r.run_id}
					subtitle={r.engine_id}
					timestamp={`${RUN_STATUS_LABEL[r.status] ?? r.status} · ${relativeTime(r.ended_at ?? r.last_seen_at ?? r.started_at)}`}
				/>
			))}
		</Tile>
	);
}

// ─── waiting on you (permissions) ──────────────────────────────────────────
//
// "Waiting on you" = permission rows whose ask is not over yet
// (`resolvedAt == null`, WP-40) — independent of read state: a pending ask
// the user glanced at in the bell is still waiting. The mock
// (designs/onboarding.html `renderDash`) answers inline; here that is done
// as far as the row's action allows:
//   - `permission.decide` (the held hooks gate): Allow once / Deny, posted to
//     `/iyke/hooks/decision` like the permission inbox. The mock's "Always
//     for this project" has no backend path for a gate answer, so it is not
//     offered.
//   - `open.terminal` / `open.thread` (Claude Code's own terminal prompt, ACP
//     asks): answered where they were asked — Open goes there.
// Opening never marks the row read; the row leaves this tile when the ask
// resolves, not when it is looked at.

/** Enough rows that a burst of resolved asks doesn't hide a pending one. */
const PERMISSIONS_FETCH_LIMIT = 50;

export function pendingPermissions(rows: readonly NotificationRow[]): NotificationRow[] {
	return rows.filter((row) => row.kind === 'permission' && !isNotificationResolved(row));
}

function usePendingPermissions() {
	const query = useQuery(
		notificationsListQueryOptions({ kinds: ['permission'], limit: PERMISSIONS_FETCH_LIMIT })
	);
	const pending = query.data ? pendingPermissions(query.data) : undefined;
	return { ...query, pending };
}

function openTerminalPane(sessionId: string): void {
	const { focusedId, addTab } = usePaneStore.getState();
	addTab(focusedId, { kind: 'terminal', sessionId });
}

function postHookDecision(requestId: string, decision: 'approved' | 'denied'): Promise<unknown> {
	return iykeFetch('/iyke/hooks/decision', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ requestId, decision }),
	}).catch(() => {});
}

function PermissionActions({ row, onDecided }: { row: NotificationRow; onDecided: () => void }) {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const decision = notificationDecision(row);

	if (decision) {
		const decide = async (d: 'approved' | 'denied') => {
			if (d === 'denied') {
				const ok = await confirmDialog(
					'Claude will get a refusal for this tool call and continue. It may ask again. Denying does not stop the session.',
					{ title: 'Deny this request?', kind: 'warning', okLabel: 'Deny' }
				);
				if (!ok) return;
			}
			await postHookDecision(decision.requestId, d);
			onDecided();
		};
		return (
			<span className="mr-1 flex shrink-0 items-center gap-1">
				<Button type="button" size="sm" onClick={() => void decide('approved')}>
					Allow once
				</Button>
				<Button type="button" size="sm" variant="outline" onClick={() => void decide('denied')}>
					Deny
				</Button>
			</span>
		);
	}

	const action = asKnownNotificationAction(row.action);
	const open = () => {
		if (action?.kind === 'open.terminal') {
			const id = action.terminalId ?? action.sessionId;
			if (id) return openTerminalPane(id);
		} else if (action?.kind === 'open.thread' && action.threadId) {
			return openTerminalPane(action.threadId);
		}
		navigateFocused('/chi');
	};
	return (
		<Button type="button" size="sm" variant="outline" className="mr-1" onClick={open}>
			Open
		</Button>
	);
}

function PermissionsTile() {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const qc = useQueryClient();
	// Hidden right after a decision so the row doesn't linger until the
	// backend's resolve round-trips back through `notifications://changed`.
	const [decided, setDecided] = useState<ReadonlySet<number>>(() => new Set());
	const { pending, isLoading, isError, refetch } = usePendingPermissions();

	if (isLoading) {
		return <LoadingState data-state="daily-address-permissions-loading" heading="Checking permissions…" />;
	}
	if (isError) {
		return (
			<ErrorState
				data-state="daily-address-permissions-error"
				heading="Couldn't load pending permissions"
				action={{ label: 'Retry', onClick: () => void refetch() }}
			/>
		);
	}

	const rows = (pending ?? []).filter((r) => !decided.has(r.id));
	if (rows.length === 0) {
		return (
			<EmptyState
				data-state="daily-address-permissions-empty"
				icon={ShieldAlert}
				heading="Nothing pending"
				body="Requests appear here and in the Companion."
				action={{ label: 'Open Companion', onClick: () => navigateFocused('/chi') }}
			/>
		);
	}

	return (
		<Tile title="Waiting on you" count={`${rows.length} permission${rows.length === 1 ? '' : 's'}`}>
			{rows.slice(0, 5).map((row: NotificationRow) => (
				<Row
					key={row.id}
					name={row.title}
					subtitle={row.body ?? undefined}
					timestamp={relativeTime(new Date(row.updatedAt).toISOString())}
					actions={
						<PermissionActions
							row={row}
							onDecided={() => {
								setDecided((prev) => new Set(prev).add(row.id));
								void invalidateNotifications(qc);
							}}
						/>
					}
				/>
			))}
		</Tile>
	);
}

// ─── updates ────────────────────────────────────────────────────────────────

function UpdatesTile() {
	const markAllRead = useMarkAllNotificationsRead();
	const updater = useUpdater({ autoPoll: false });
	const derived = usePkgsDerived();
	const updatePkgs = useUpdatePkgs();
	const { data, isLoading, isError, refetch } = useQuery(
		notificationsListQueryOptions({ kinds: ['update'], unreadOnly: true, limit: 5 })
	);

	if (isLoading) {
		return <LoadingState data-state="daily-address-updates-loading" heading="Checking for updates…" />;
	}
	if (isError) {
		return (
			<ErrorState
				data-state="daily-address-updates-error"
				heading="Couldn't load updates"
				action={{ label: 'Retry', onClick: () => void refetch() }}
			/>
		);
	}

	const rows = data ?? [];
	const pkgUpdates = derived.updates;
	const nothingToShow = rows.length === 0 && !updater.available && pkgUpdates.length === 0;
	// Both live sources (the pkg registry cross-reference, the app-binary
	// check) resolve after mount — don't report "up to date" before either
	// has actually checked, or a real update can flash as absent for a beat.
	if (nothingToShow && (derived.isLoading || updater.checking)) {
		return <LoadingState data-state="daily-address-updates-loading" heading="Checking for updates…" />;
	}
	if (nothingToShow) {
		return (
			<EmptyState
				data-state="daily-address-updates-empty"
				icon={RefreshCw}
				heading="Everything is up to date"
				action={{ label: 'Check for updates', onClick: () => void updater.check() }}
			/>
		);
	}

	const shellRow = updater.available
		? `shell · ${updater.available.currentVersion ?? ''} → ${updater.available.version}`.trim()
		: null;

	return (
		<Tile
			title="Updates"
			count={`${rows.length} unread`}
			footer={
				<>
					{(updater.available || pkgUpdates.length > 0) && (
						<Button
							type="button"
							size="sm"
							variant="outline"
							disabled={updater.installing || updatePkgs.isPending}
							onClick={() => {
								if (pkgUpdates.length > 0) updatePkgs.mutate({ rows: pkgUpdates });
								if (updater.available) void updater.install();
							}}
						>
							{updater.installed ? 'Restart to finish' : 'Update all'}
						</Button>
					)}
					<span className="flex-1" />
					{rows.length > 0 && (
						<button
							type="button"
							className="underline-offset-2 hover:underline"
							onClick={() => markAllRead.mutate('update')}
						>
							Mark all read
						</button>
					)}
				</>
			}
		>
			{shellRow && <Row name={shellRow} subtitle="app update" />}
			{rows.map((row: NotificationRow) => (
				<Row key={row.id} name={row.title} subtitle={row.body ?? undefined} />
			))}
			{updater.installed && (
				<Row
					name="Installed — restart to finish"
					actions={
						<Button type="button" size="sm" variant="outline" onClick={() => void updater.restart()}>
							Restart
						</Button>
					}
				/>
			)}
		</Tile>
	);
}

// ─── todos ──────────────────────────────────────────────────────────────────

const OPEN_TODO_STATUSES = new Set(['open', 'in_progress', 'blocked']);

function useProjectTodos() {
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const queryKey = queryKeys.dailyAddress.todos(activeProjectId);
	const query = useQuery({
		queryKey,
		queryFn: async () => {
			const res = await listTodos({ scope: `project:${activeProjectId}` });
			return res?.todos ?? [];
		},
	});
	const open = query.data?.filter((t) => OPEN_TODO_STATUSES.has(t.status));
	return { ...query, queryKey, open };
}

function TodosTile() {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const qc = useQueryClient();
	const { queryKey, open: allOpen, isLoading, isError, refetch } = useProjectTodos();
	const complete = useMutation({
		mutationFn: (id: string) => completeTodo(id),
		onSuccess: () => void qc.invalidateQueries({ queryKey }),
	});

	if (isLoading) {
		return <LoadingState data-state="daily-address-todos-loading" heading="Loading todos…" />;
	}
	if (isError) {
		return (
			<ErrorState
				data-state="daily-address-todos-error"
				heading="Couldn't load todos"
				action={{ label: 'Retry', onClick: () => void refetch() }}
			/>
		);
	}

	const openCount = allOpen?.length ?? 0;
	const open = (allOpen ?? []).slice(0, 4);
	if (open.length === 0) {
		return (
			<EmptyState
				data-state="daily-address-todos-empty"
				icon={ListChecks}
				heading="Nothing due"
				action={{ label: 'Open Tasks', onClick: () => navigateFocused('/todos') }}
			/>
		);
	}

	return (
		<Tile
			title="Todos"
			count={`${openCount} open`}
			footer={
				<button
					type="button"
					className="underline-offset-2 hover:underline"
					onClick={() => navigateFocused('/todos')}
				>
					Open Tasks
				</button>
			}
		>
			{open.map((t: Todo) => (
				<Row
					key={t.id}
					name={t.title}
					subtitle={t.status.replace('_', ' ')}
					actions={
						<Button
							type="button"
							size="sm"
							variant="outline"
							className="mr-1"
							onClick={(e) => {
								e.stopPropagation();
								complete.mutate(t.id);
							}}
						>
							Done
						</Button>
					}
				/>
			))}
		</Tile>
	);
}

// ─── greeting ───────────────────────────────────────────────────────────────

export function greetingFor(hour: number): string {
	if (hour < 12) return 'Good morning';
	if (hour < 18) return 'Good afternoon';
	return 'Good evening';
}

function plural(n: number, one: string, many = `${one}s`): string {
	return `${n} ${n === 1 ? one : many}`;
}

/**
 * The line under the greeting, from whatever has loaded (a part whose source
 * is still loading or failed is left out rather than guessed). The mock's
 * "Yesterday your Chi ran four things, one is still going, and four todos
 * are open." `null` when nothing has loaded yet.
 */
export function summarizeDay(parts: {
	runs?: readonly ChiCacheRow[];
	pending?: number;
	openTodos?: number;
}): string | null {
	const bits: string[] = [];
	if (parts.runs) {
		const active = parts.runs.filter((r) => r.status === 'running' || r.status === 'queued').length;
		const finished = parts.runs.length - active;
		if (parts.runs.length === 0) bits.push('no runs since yesterday');
		else {
			bits.push(`your Chi ran ${plural(finished, 'thing')} since yesterday`);
			if (active > 0) bits.push(`${active} ${active === 1 ? 'is' : 'are'} still going`);
		}
	}
	if (parts.pending != null && parts.pending > 0) {
		bits.push(`${plural(parts.pending, 'permission')} ${parts.pending === 1 ? 'is' : 'are'} waiting on you`);
	}
	if (parts.openTodos != null) {
		bits.push(`${plural(parts.openTodos, 'todo')} ${parts.openTodos === 1 ? 'is' : 'are'} open`);
	}
	if (bits.length === 0) return null;
	const sentence =
		bits.length === 1 ? bits[0]! : `${bits.slice(0, -1).join(', ')}, and ${bits[bits.length - 1]}`;
	return `${sentence.charAt(0).toUpperCase()}${sentence.slice(1)}.`;
}

function Greeting() {
	const userName = useShellStore((s) => s.userName);
	const { runs } = useProjectRuns();
	const { pending } = usePendingPermissions();
	const { open } = useProjectTodos();
	const summary = summarizeDay({ runs, pending: pending?.length, openTodos: open?.length });
	const name = userName?.trim();
	return (
		<h1 className="flex flex-col gap-0.5 text-base font-semibold text-foreground">
			<span>
				{greetingFor(new Date().getHours())}
				{name ? `, ${name}` : ''}.
			</span>
			{summary && (
				<span data-testid="daily-address-summary" className="text-xs font-normal text-muted-foreground">
					{summary}
				</span>
			)}
		</h1>
	);
}

// ─── the address ────────────────────────────────────────────────────────────

/** Same key the settings shell reads under, so its `settings://changed`
 *  watcher and its post-write `refresh()` (both invalidate `['settings',
 *  'file']`) keep this in step. */
const PERSONAL_SETTINGS_QK = ['settings', 'file', 'personal', null] as const;

/** Whether the daily address renders right now: not dismissed today, and
 *  the Workspace setting is on. The dashboard also reads this so the Obi
 *  canvas can drop its own greeting while the address is up (D-04 has one
 *  greeting, not two). */
export function useDailyAddressShown(): boolean {
	const dismissedOn = useShellStore((s) => s.dailyAddressDismissedOn);
	const settings = useQuery({
		queryKey: PERSONAL_SETTINGS_QK,
		queryFn: () => readSettingsFile({ scope: 'personal', projectId: null }),
		staleTime: 15_000,
	});
	if (dismissedOn === todayLocalDate()) return false;
	// Wait for the setting rather than flash an address the user turned off.
	// A read failure falls back to the default (on).
	if (settings.isLoading) return false;
	return isDailyAddressEnabled(settings.data?.effective);
}

export function DailyAddress() {
	const setDismissed = useShellStore((s) => s.setDailyAddressDismissed);
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const navigate = useNavigate();
	const today = todayLocalDate();
	const shown = useDailyAddressShown();

	if (!shown) return null;

	return (
		<section
			data-state="daily-address"
			aria-label="Daily address"
			className={cn('mb-4 flex flex-col gap-2 rounded-[var(--radius-md)] border border-border bg-card p-3')}
		>
			<div className="flex items-start justify-between gap-2 px-1">
				<Greeting />
				<button
					type="button"
					aria-label="Dismiss daily address"
					className="grid size-11 shrink-0 place-items-center rounded text-muted-foreground outline-none hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring"
					onClick={() => setDismissed(today)}
				>
					<X aria-hidden className="size-4" />
				</button>
			</div>
			<div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-4">
				<RunsTile />
				<PermissionsTile />
				<UpdatesTile />
				<TodosTile />
			</div>
			<div className="flex flex-wrap items-center gap-2 px-1 pt-1 text-[11px] text-muted-foreground">
				<Inbox aria-hidden className="size-3.5" />
				<span>Assembled at app open, read-only. Turn it off in Settings › Workspace.</span>
				<span className="flex-1" />
				<Button
					type="button"
					size="sm"
					variant="outline"
					onClick={() => navigateFocused('/settings/workspace')}
				>
					<SettingsIcon aria-hidden className="mr-1 size-3.5" />
					Settings › Workspace
				</Button>
				<Button
					type="button"
					size="sm"
					variant="outline"
					onClick={() => void runConsecrationAgain(() => void navigate({ to: '/onboarding' }))}
				>
					<Sparkles aria-hidden className="mr-1 size-3.5" />
					Run consecration again
				</Button>
				<span className="font-mono text-[10px] text-muted-foreground/70">
					iyke go /project/dashboard
				</span>
			</div>
		</section>
	);
}
