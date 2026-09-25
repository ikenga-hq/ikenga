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
//   runs          | cached Chi runs + Claude session history   | chiList() (chi.rs `chi_list`)
//   permissions   | WP-40 notifications, kind=permission       | notificationsListQueryOptions()
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
// there for why this uses the shell-store rather than the settings.json
// client.

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
	Clock,
	Inbox,
	ListChecks,
	RefreshCw,
	ShieldAlert,
	X,
} from 'lucide-react';
import type { ReactNode } from 'react';
import { EmptyState, ErrorState, LoadingState } from '@/components/states';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardFooter, CardHeader, CardTitle } from '@/components/ui/card';
import { cn } from '@/components/ui/utils';
import { completeTodo, listTodos, type Todo } from '@/lib/iyke/memory';
import { usePaneStore } from '@/lib/panes/pane-store';
import { usePkgsDerived } from '@/lib/pkgs/use-derived';
import { useUpdatePkgs } from '@/lib/pkgs/use-update-pkgs';
import { queryKeys } from '@/lib/query-keys';
import {
	notificationsListQueryOptions,
	useMarkAllNotificationsRead,
	useMarkNotificationsRead,
} from '@/lib/queries/notifications';
import { useShellStore } from '@/lib/shell/shell-store';
import { chiList, type ChiCacheRow, type NotificationRow } from '@/lib/tauri-cmd';
import { useUpdater } from '@/lib/updater/use-updater';

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

function RunsTile() {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const { data, isLoading, isError, refetch } = useQuery({
		queryKey: queryKeys.dailyAddress.runs(),
		queryFn: () => chiList(null, 30),
		staleTime: 30_000,
	});

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

	const recent = (data ?? []).filter(isRecentOrActive).slice(0, 6);
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
			count={`${recent.length} run${recent.length === 1 ? '' : 's'}`}
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

function PermissionsTile() {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const markRead = useMarkNotificationsRead();
	const { data, isLoading, isError, refetch } = useQuery(
		notificationsListQueryOptions({ kinds: ['permission'], unreadOnly: true, limit: 5 })
	);

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

	const rows = data ?? [];
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
			{rows.map((row: NotificationRow) => (
				<Row
					key={row.id}
					name={row.title}
					subtitle={row.body ?? undefined}
					timestamp={relativeTime(new Date(row.updatedAt).toISOString())}
					actions={
						<Button
							type="button"
							size="sm"
							variant="outline"
							className="mr-1"
							onClick={(e) => {
								e.stopPropagation();
								navigateFocused('/chi');
								markRead.mutate([row.id]);
							}}
						>
							Open
						</Button>
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

function TodosTile() {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const qc = useQueryClient();
	const queryKey = queryKeys.dailyAddress.todos(activeProjectId);
	const { data, isLoading, isError, refetch } = useQuery({
		queryKey,
		queryFn: async () => {
			const res = await listTodos({ scope: `project:${activeProjectId}` });
			return res?.todos ?? [];
		},
	});
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

	const open = (data ?? []).filter((t) => OPEN_TODO_STATUSES.has(t.status)).slice(0, 4);
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
			count={`${open.length} open`}
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

// ─── the address ────────────────────────────────────────────────────────────

export function DailyAddress() {
	const dismissedOn = useShellStore((s) => s.dailyAddressDismissedOn);
	const setDismissed = useShellStore((s) => s.setDailyAddressDismissed);
	const today = todayLocalDate();

	if (dismissedOn === today) return null;

	return (
		<section
			data-state="daily-address"
			aria-label="Daily address"
			className={cn('mb-4 flex flex-col gap-2 rounded-[var(--radius-md)] border border-border bg-card p-3')}
		>
			<div className="flex items-center justify-between px-1">
				<h2 className="text-sm font-semibold text-foreground">Daily address</h2>
				<button
					type="button"
					aria-label="Dismiss daily address"
					className="grid size-11 place-items-center rounded text-muted-foreground outline-none hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring"
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
				<span>
					Assembled at app open, read-only. Turn it off, or change what it counts, in Settings ›
					Workspace.
				</span>
				<span className="flex-1" />
				<span className="font-mono text-[10px] text-muted-foreground/70">
					iyke go /project/dashboard
				</span>
			</div>
		</section>
	);
}
