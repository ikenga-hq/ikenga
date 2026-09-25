// WP-40b — D-07 `notifications` state: the popover behind the status-bar
// bell (`bell.tsx`). Grouped Today / Earlier (`groupNotificationsByDay`,
// WP-40's own view helper), a per-row action (`./actions.ts`), mark all
// read, per-kind mute (permission/violation excluded — D-07: "Permission
// and violation cannot be muted; every other kind can"), and a link to the
// Settings section that owns `workspace.notifications.mutedKinds`
// (`src/shell/settings/nav.tsx` section `workspace`, per
// `src/lib/settings/types.ts`).
//
// designs/system-flows.html?state=notifications

import { useQuery } from '@tanstack/react-query';
import { Bell, MoreHorizontal } from 'lucide-react';
import { useMemo } from 'react';
import { EmptyState } from '@/components/states';
import {
	DropdownMenu,
	DropdownMenuCheckboxItem,
	DropdownMenuContent,
	DropdownMenuSeparator,
	DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { cn } from '@/components/ui/utils';
import { usePaneStore } from '@/lib/panes/pane-store';
import {
	groupNotificationsByDay,
	MUTABLE_NOTIFICATION_KINDS,
	notificationsListQueryOptions,
	notificationsMuteStateQueryOptions,
	useMarkAllNotificationsRead,
	useMarkNotificationsRead,
	useSetNotificationKindMuted,
} from '@/lib/queries/notifications';
import { formatRelativeTime } from '@/lib/relative-time';
import type { NotificationKind, NotificationRow } from '@/lib/tauri-cmd';
import { notificationActionButtons } from './actions';

/** Where the Workspace settings section (which owns
 *  `workspace.notifications.mutedKinds`) lives. */
const NOTIFICATION_SETTINGS_ROUTE = '/settings/workspace';

export const KIND_META: Record<NotificationKind, { label: string; fg: string; border: string }> = {
	permission: { label: 'permission', fg: 'var(--achievement)', border: 'var(--achievement-soft)' },
	violation: { label: 'violation', fg: 'var(--danger)', border: 'var(--danger-soft)' },
	run_failed: { label: 'run failed', fg: 'var(--danger)', border: 'var(--danger-soft)' },
	run_finished: { label: 'run finished', fg: 'var(--live)', border: 'var(--live-soft)' },
	update: { label: 'update', fg: 'var(--info)', border: 'var(--info-soft)' },
	invite: { label: 'invite', fg: 'var(--agent)', border: 'var(--agent-soft)' },
};

function KindTag({ kind }: { kind: NotificationKind }) {
	const meta = KIND_META[kind];
	return (
		<span
			className="inline-flex h-4 shrink-0 items-center rounded-[var(--radius-xs)] border px-1.5 font-mono text-[10px] tracking-wide"
			style={{ color: meta.fg, borderColor: meta.border }}
		>
			{meta.label}
		</span>
	);
}

function NotificationRowItem({ row }: { row: NotificationRow }) {
	const markRead = useMarkNotificationsRead();
	const buttons = useMemo(() => notificationActionButtons(row), [row]);
	const unread = row.readAt == null;

	function act(button: ReturnType<typeof notificationActionButtons>[number]) {
		button.run();
		if (unread) markRead.mutate([row.id]);
	}

	return (
		<div
			data-notification-row={row.id}
			className={cn(
				'flex flex-col gap-1 border-b border-border-soft/60 px-3 py-2 last:border-b-0',
				unread && 'bg-[var(--bg-raised)]'
			)}
		>
			<div className="flex items-start gap-2">
				<KindTag kind={row.kind} />
				<span className="min-w-0 flex-1 text-xs leading-snug text-foreground" title={row.title}>
					{row.title}
				</span>
				<span className="shrink-0 pt-px font-mono text-[10px] text-muted-foreground">
					{formatRelativeTime(row.updatedAt)}
				</span>
			</div>
			{row.body && <p className="truncate text-[11px] text-muted-foreground">{row.body}</p>}
			{buttons.length > 0 && (
				<div className="flex items-center gap-1.5 pt-0.5">
					{buttons.map((button) => (
						<button
							key={button.label}
							type="button"
							onClick={() => act(button)}
							className={cn(
								'flex min-h-[var(--tab-h)] items-center justify-center rounded-[var(--radius-xs)] border px-2 font-mono text-[10px] outline-none transition-colors motion-reduce:transition-none',
								'focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset',
								button.variant === 'primary'
									? 'border-transparent bg-[var(--achievement)] text-[var(--bg-base)] hover:opacity-90'
									: 'border-border-soft text-foreground hover:bg-accent'
							)}
						>
							{button.label}
						</button>
					))}
				</div>
			)}
		</div>
	);
}

function GroupHeader({ label }: { label: string }) {
	return (
		<div className="sticky top-0 z-10 bg-[var(--bg-raised)] px-3 py-1 font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
			{label}
		</div>
	);
}

export interface NotificationsPopoverContentProps {
	/** Closes the popover — the footer link and empty-state action navigate
	 *  away, so the trigger's own `onOpenChange` won't fire from a route
	 *  change inside a different pane. */
	onClose: () => void;
}

export function NotificationsPopoverContent({ onClose }: NotificationsPopoverContentProps) {
	const listQuery = useQuery(notificationsListQueryOptions({ limit: 50 }));
	const muteQuery = useQuery(notificationsMuteStateQueryOptions());
	const markAllRead = useMarkAllNotificationsRead();
	const setKindMuted = useSetNotificationKindMuted();

	const rows = listQuery.data ?? [];
	const { today, earlier } = useMemo(() => groupNotificationsByDay(rows), [rows]);
	const hasAny = rows.length > 0;
	const muted = new Set(muteQuery.data?.muted ?? []);

	function openNotificationSettings() {
		onClose();
		usePaneStore.getState().navigateFocused(NOTIFICATION_SETTINGS_ROUTE);
	}

	return (
		<div className="flex max-h-[70vh] flex-col" data-state="notifications">
			<header className="flex items-center gap-1 border-b border-border-soft px-3 py-2">
				<span className="text-xs font-semibold text-foreground">Notifications</span>
				<span className="flex-1" />
				<button
					type="button"
					onClick={() => markAllRead.mutate(null)}
					disabled={!hasAny || markAllRead.isPending}
					className="min-h-[var(--tab-h)] rounded px-2 font-mono text-[10px] text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground disabled:pointer-events-none disabled:opacity-40"
				>
					Mark all read
				</button>
				<DropdownMenu>
					<DropdownMenuTrigger asChild>
						<button
							type="button"
							aria-label="Mute a kind"
							aria-haspopup="menu"
							className="flex min-h-[var(--tab-h)] min-w-[var(--tab-h)] items-center justify-center rounded text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground"
						>
							<MoreHorizontal aria-hidden className="h-3.5 w-3.5" />
						</button>
					</DropdownMenuTrigger>
					<DropdownMenuContent align="end" className="w-60">
						<div className="px-2 py-1 font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
							Mute a kind
						</div>
						{MUTABLE_NOTIFICATION_KINDS.map((kind) => (
							<DropdownMenuCheckboxItem
								key={kind}
								checked={muted.has(kind)}
								onCheckedChange={(checked) => setKindMuted.mutate({ kind, muted: checked === true })}
							>
								{KIND_META[kind].label}
							</DropdownMenuCheckboxItem>
						))}
						<DropdownMenuSeparator />
						<div className="px-2 py-1.5 text-[10px] leading-snug text-muted-foreground">
							Permission and violation cannot be muted.
						</div>
					</DropdownMenuContent>
				</DropdownMenu>
			</header>

			{!hasAny ? (
				<EmptyState
					data-state="notifications-empty"
					icon={Bell}
					heading="Nothing yet"
					body="Permission requests, run results, updates, violations and invites collect here."
					action={{ label: 'Notification settings', onClick: openNotificationSettings }}
				/>
			) : (
				<div className="flex-1 overflow-y-auto">
					{today.length > 0 && (
						<div>
							<GroupHeader label="Today" />
							{today.map((row) => (
								<NotificationRowItem key={row.id} row={row} />
							))}
						</div>
					)}
					{earlier.length > 0 && (
						<div>
							<GroupHeader label="Earlier" />
							{earlier.map((row) => (
								<NotificationRowItem key={row.id} row={row} />
							))}
						</div>
					)}
				</div>
			)}

			<footer className="flex items-center gap-2 border-t border-border-soft px-3 py-1.5">
				<span className="truncate font-mono text-[10px] text-muted-foreground">
					~/.ikenga/ikenga.db · notifications
				</span>
				<span className="flex-1" />
				<button
					type="button"
					onClick={openNotificationSettings}
					className="min-h-[var(--tab-h)] whitespace-nowrap rounded px-2 font-mono text-[10px] text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground"
				>
					Notification settings →
				</button>
			</footer>
		</div>
	);
}
