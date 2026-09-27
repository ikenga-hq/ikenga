// biome-ignore-all lint/a11y/useSemanticElements: ListRow is a div[role=button] on purpose — it hosts nested action <button>s (RowAction), and a <button> can't nest in a <button> (the tab-strip R5 parser-ejection lesson). A real <button> would be invalid HTML here.
import { forwardRef, type HTMLAttributes, type ReactNode } from 'react';
import { cva, type VariantProps } from 'class-variance-authority';

import { cn } from '@/components/ui/utils';

// ListRow — the shared sidebar list-row primitive. Consolidates the duplicated
// `flex items-center gap-2 px-N py-1.5 hover:bg-accent` rows across the sidebar
// modes (sessions ThreadRow · artifact-grid RecentRow/RecentArtifactRow · files
// TreeNode) into one row container that owns the things they had drifted on:
//   • the row geometry (size sm|md|lg) + selected state
//   • a `focus-visible` ring + `motion-reduce` guard (all three lacked both)
//   • the `<div role="button">` shell so hover-action `<button>`s can be REAL,
//     keyboard-operable buttons — a `<button>` can't nest in a `<button>`, the
//     same parser-ejection lesson tab-strip applied (tab-strip.md R5)
//   • the gold unread badge → `var(--achievement)` (kills the `#f59e0b` bypass)
// Per-site composition (chevron + rename input + context-menu for files; color
// dot + subtitle for sessions; trash action for recents) stays at the call site
// via the `children`/`actions` slots — only the shell + a11y consolidate here.
//
// Spec: plans/shell-design-system/parts/components/list-row.md §3–§4
//       + designs/list-row.html (the locked Dusk Wood mockup).

export const listRowClass = cva(
	'group/row relative flex w-full items-center gap-2 text-left transition-colors motion-reduce:transition-none ' +
		'outline-none focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-[-2px] focus-visible:outline-primary ' +
		'text-muted-foreground hover:bg-accent hover:text-accent-foreground',
	{
		variants: {
			size: {
				sm: 'px-2 py-1 text-xs', // files-mode (dense tree)
				md: 'px-3 py-1.5 text-xs', // sessions
				lg: 'px-4 py-1.5 text-sm', // artifact-grid recents
			},
			selected: {
				true: 'bg-accent font-medium text-accent-foreground',
				false: '',
			},
		},
		defaultVariants: { size: 'md', selected: false },
	}
);

export interface ListRowProps
	extends Omit<HTMLAttributes<HTMLDivElement>, 'onSelect'>,
		VariantProps<typeof listRowClass> {
	/** Called on click and on Enter/Space when the row itself is focused. */
	onActivate?: () => void;
	/** px paddingLeft override (files-mode depth indent). */
	indent?: number;
	/** Leading glyph (Lucide icon, color dot, chevron cluster…). */
	icon?: ReactNode;
	/** Primary label — rendered `text-foreground` (truncated). */
	name?: ReactNode;
	/** Muted secondary line beneath the name (sessions cwd). */
	subtitle?: ReactNode;
	/** Right-aligned mono timestamp. */
	timestamp?: ReactNode;
	/** Trailing badge (e.g. <UnreadBadge/>), before the timestamp. */
	badge?: ReactNode;
	/** Hover/focus-revealed action buttons (real <button>s — see RowAction). */
	actions?: ReactNode;
	/** Full control of the middle content (files-mode chevron+icon+rename). When
	 *  set, `name`/`subtitle`/`timestamp`/`badge` are ignored. */
	children?: ReactNode;
}

export const ListRow = forwardRef<HTMLDivElement, ListRowProps>(function ListRow(
	{
		size,
		selected,
		onActivate,
		indent,
		icon,
		name,
		subtitle,
		timestamp,
		badge,
		actions,
		children,
		className,
		style,
		...rest
	},
	ref
) {
	return (
		<div
			{...rest}
			ref={ref}
			role="button"
			tabIndex={0}
			onClick={() => onActivate?.()}
			onKeyDown={(e) => {
				// Only the row itself activates on Enter/Space — descendants
				// (rename input, action buttons) keep their own key handling.
				if (e.target === e.currentTarget && (e.key === 'Enter' || e.key === ' ')) {
					e.preventDefault();
					onActivate?.();
				}
			}}
			className={cn(listRowClass({ size, selected }), className)}
			style={indent != null ? { paddingLeft: `${indent}px`, ...style } : style}
		>
			{children ?? (
				<>
					{icon}
					{subtitle != null ? (
						<div className="flex min-w-0 flex-1 flex-col">
							<span className="truncate font-medium text-foreground">{name}</span>
							<span className="truncate text-[10px] text-muted-foreground/80">{subtitle}</span>
						</div>
					) : (
						<span className="min-w-0 flex-1 truncate text-foreground">{name}</span>
					)}
					{badge}
					{timestamp != null && (
						<span className="shrink-0 font-mono text-[10px] tabular-nums text-muted-foreground">
							{timestamp}
						</span>
					)}
				</>
			)}
			{actions}
		</div>
	);
});

/** Gold unread-count badge. `var(--achievement)` + self-contrasting
 *  `var(--achievement-soft)` ink (the WP-A gold-button pair) — kills the old
 *  `bg-[var(--accent,#f59e0b)] text-white` bypass. Renders nothing at 0. */
export function UnreadBadge({ count }: { count: number }) {
	if (count <= 0) return null;
	return (
		<span
			role="img"
			aria-label={`${count} unread`}
			className="grid h-4 min-w-4 shrink-0 place-items-center rounded-full px-1 text-[9px] font-semibold leading-none"
			style={{ background: 'var(--achievement)', color: 'var(--achievement-soft)' }}
		>
			{count > 9 ? '9+' : count}
		</span>
	);
}

export interface RowActionProps {
	icon: ReactNode;
	label: string;
	onClick: () => void;
	/** Hover ink → destructive (trash). */
	danger?: boolean;
	className?: string;
}

/** A hover/focus-revealed row-action button. Reveals on row hover AND on
 *  keyboard focus-within (the originals used `invisible group-hover:visible`,
 *  which left them keyboard-unreachable). Stops propagation so it doesn't
 *  trigger the row's onActivate. 24×24 target (WCAG 2.5.8). */
export function RowAction({ icon, label, onClick, danger, className }: RowActionProps) {
	return (
		<button
			type="button"
			aria-label={label}
			title={label}
			onClick={(e) => {
				e.stopPropagation();
				onClick();
			}}
			className={cn(
				'grid size-6 shrink-0 place-items-center rounded text-muted-foreground',
				'opacity-0 transition-opacity group-hover/row:opacity-100 group-focus-within/row:opacity-100 focus-visible:opacity-100',
				'outline-none hover:bg-background hover:text-foreground',
				'focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset',
				'motion-reduce:transition-none',
				danger && 'hover:text-destructive',
				className
			)}
		>
			{icon}
		</button>
	);
}
