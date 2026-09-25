import { AlertTriangle, CheckCircle2, Download, ShieldAlert, UserPlus, X, XCircle } from 'lucide-react';
import * as React from 'react';
import { cn } from '@/components/ui/utils';
import { useNotificationsLiveSync } from '@/lib/queries/notifications';
import type { NotificationKind, NotificationRow } from '@/lib/tauri-cmd';

export type FloatingToastChipVariant = 'progress' | 'error' | 'notice' | 'info';
export type FloatingToastChipAnchor = 'viewport-top' | 'pane-corner';

// Border color per variant — replaces the hand-rolled `border-amber-500/40`
// bypass with the Dusk Wood gold (`--achievement`) for the in-progress / notice
// lane, `--danger` for error, and the neutral `--border` for info.
const VARIANT_BORDER: Record<FloatingToastChipVariant, string> = {
	progress: 'color-mix(in srgb, var(--achievement) 30%, transparent)',
	notice: 'color-mix(in srgb, var(--achievement) 30%, transparent)',
	error: 'var(--danger)',
	info: 'var(--border)',
};

// Error chips are assertive; everything else is polite.
const VARIANT_ROLE: Record<FloatingToastChipVariant, 'status' | 'alert'> = {
	progress: 'status',
	notice: 'status',
	info: 'status',
	error: 'alert',
};

const WRAP: Record<FloatingToastChipAnchor, string> = {
	'viewport-top': 'pointer-events-none fixed inset-x-0 top-2 z-40 flex justify-center',
	'pane-corner': 'pointer-events-none absolute right-1.5 top-1.5 z-10 flex',
};

/**
 * The shared pill className. Exported so bespoke chips whose *whole pill* is the
 * click target (the iyke corner chip lives inside the pane-iyke overlay beside
 * its shimmer line and opens a logs sheet) get the same shape + focus ring
 * without contorting into the prop API. Set the border color via inline
 * `style={{ borderColor: VARIANT_BORDER[variant] }}` (or `var(--border)`).
 */
export function floatingChipPill(anchor: FloatingToastChipAnchor, interactive = false): string {
	return cn(
		'pointer-events-auto flex items-center rounded-md border backdrop-blur',
		anchor === 'pane-corner'
			? 'gap-1.5 bg-background/80 px-1.5 py-1 text-[10px] font-medium leading-none shadow-sm'
			: 'max-w-md gap-2 bg-background/95 px-3 py-1.5 text-xs shadow-lg',
		interactive &&
			'outline-none transition-colors motion-reduce:transition-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset'
	);
}

const ctaClass =
	'flex items-center gap-1 rounded border border-border bg-background px-2 py-1 text-foreground outline-none transition-colors hover:bg-accent motion-reduce:transition-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset';

export interface FloatingToastChipProps {
	variant?: FloatingToastChipVariant;
	anchor?: FloatingToastChipAnchor;
	/** Lucide icon node (rendered muted, or `--danger` on the error variant). */
	icon?: React.ReactNode;
	/** Message — `ReactNode` so callers can embed a `<code>` filename etc. */
	label: React.ReactNode;
	/** Single trailing CTA button (Retry / Open folder / …). */
	action?: { label: string; icon?: React.ReactNode; onClick: () => void };
	/** When present, renders a trailing keyboard-reachable dismiss `×`. */
	onDismiss?: () => void;
	/** Auto-dismiss after this many ms (generic transient path; fires `onDismiss`). */
	ttlMs?: number;
	className?: string;
}

/**
 * A small, non-blocking, transient status pill that floats above shell content
 * without stealing focus or pointer events (the wrapper is `pointer-events-none`;
 * only the pill is `pointer-events-auto`). Consolidates the runtime-bun and
 * wizard-recovery viewport chips — replacing their hardcoded amber border with
 * the variant token map, adding `focus-visible` rings on the CTAs, a `role`
 * `status`/`alert` live region, and a reduced-motion-aware entrance.
 */
export function FloatingToastChip({
	variant = 'info',
	anchor = 'viewport-top',
	icon,
	label,
	action,
	onDismiss,
	ttlMs,
	className,
}: FloatingToastChipProps) {
	React.useEffect(() => {
		if (!ttlMs || !onDismiss) return;
		const t = setTimeout(onDismiss, ttlMs);
		return () => clearTimeout(t);
	}, [ttlMs, onDismiss]);

	const isError = variant === 'error';
	return (
		<div className={WRAP[anchor]} role={VARIANT_ROLE[variant]}>
			<div
				className={cn(
					floatingChipPill(anchor),
					'animate-in fade-in slide-in-from-top-2 motion-reduce:animate-none',
					className
				)}
				style={{ borderColor: VARIANT_BORDER[variant] }}
			>
				{icon && (
					<span
						className="flex h-3 w-3 shrink-0 items-center justify-center text-muted-foreground"
						style={isError ? { color: 'var(--danger)' } : undefined}
					>
						{icon}
					</span>
				)}
				<span className="text-foreground">{label}</span>
				{action && (
					<button type="button" onClick={action.onClick} className={ctaClass}>
						{action.icon}
						<span>{action.label}</span>
					</button>
				)}
				{onDismiss && (
					<button
						type="button"
						onClick={onDismiss}
						aria-label="Dismiss"
						className="rounded p-1 text-muted-foreground outline-none transition-colors hover:bg-accent hover:text-foreground motion-reduce:transition-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
					>
						<X className="h-3 w-3" />
					</button>
				)}
			</div>
		</div>
	);
}

// ─── WP-40b: notification toast bridge (D-07 `notifications`) ─────────────
//
// "A toast is gone in three seconds and the banner slot holds one notice.
// This is the list behind both — a toast becomes a transient copy of a row
// that stays." (designs/system-flows.html?state=notifications) The row
// itself lives in the `notifications` table / the bell popover
// (`src/shell/notifications/`); this renders a 3.2s `FloatingToastChip`
// copy of whatever `notifications://changed` just created, then discards it.
// Text-only, like the design's `toast(text, { ms: 3200 })`: no CTA. A live
// decision (Allow once / Deny) on a pill that vanishes in 3.2 s is a
// misclick trap, and every action already lives on the row in the popover
// (`src/shell/notifications/actions.ts`), which knows when an ask is over.
// Suppressed when the event's `muted` flag is set — Rust already excludes
// muted kinds from the unread count, and the toast honors the same rule
// rather than surfacing a notice for something the popover won't show as
// unread.
//
// This is the ONE place in the app that calls `useNotificationsLiveSync()`
// (`src/lib/queries/notifications.ts` asks for exactly one mount "near the
// shell root"). It is mounted once, at the top level of
// `src/shell/notifications/bell.tsx` — a sibling of the `Popover`, not
// inside `PopoverContent` (which unmounts on close and would stop hearing
// events, and would defeat the badge count staying live while the popover
// is shut).

const NOTIFICATION_TOAST_VARIANT: Record<NotificationKind, FloatingToastChipVariant> = {
	permission: 'notice',
	violation: 'error',
	run_failed: 'error',
	run_finished: 'info',
	update: 'info',
	invite: 'info',
};

const NOTIFICATION_TOAST_ICON: Record<NotificationKind, React.ReactNode> = {
	permission: <ShieldAlert className="h-3 w-3" />,
	violation: <AlertTriangle className="h-3 w-3" />,
	run_failed: <XCircle className="h-3 w-3" />,
	run_finished: <CheckCircle2 className="h-3 w-3" />,
	update: <Download className="h-3 w-3" />,
	invite: <UserPlus className="h-3 w-3" />,
};

/** designs/system-flows.html's own toast demo: `later(i * 260, () => toast(..., { ms: 3200 }))`. */
const NOTIFICATION_TOAST_TTL_MS = 3200;

interface QueuedNotificationToast {
	key: number;
	row: NotificationRow;
}

let notificationToastSeq = 0;

/** One toast on screen at a time; a burst (e.g. three violations in the same
 *  second) queues and drains at the same 3.2s cadence rather than stacking
 *  overlapping pills. */
export function NotificationToastBridge() {
	const [queue, setQueue] = React.useState<QueuedNotificationToast[]>([]);

	useNotificationsLiveSync((event) => {
		if (event.reason !== 'created' && event.reason !== 'coalesced') return;
		if (event.muted || !event.notification) return;
		notificationToastSeq += 1;
		const row = event.notification;
		setQueue((q) => [...q, { key: notificationToastSeq, row }]);
	});

	const shown = queue[0] ?? null;

	const dismiss = React.useCallback((key: number) => {
		setQueue((q) => q.filter((t) => t.key !== key));
	}, []);

	if (!shown) return null;

	return (
		<FloatingToastChip
			key={shown.key}
			variant={NOTIFICATION_TOAST_VARIANT[shown.row.kind]}
			anchor="viewport-top"
			icon={NOTIFICATION_TOAST_ICON[shown.row.kind]}
			label={shown.row.title}
			onDismiss={() => dismiss(shown.key)}
			ttlMs={NOTIFICATION_TOAST_TTL_MS}
		/>
	);
}
