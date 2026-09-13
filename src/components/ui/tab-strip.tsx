import {
	forwardRef,
	useCallback,
	useEffect,
	useRef,
	type ReactNode,
	type HTMLAttributes,
} from 'react';
import { Pin, PinOff, X } from 'lucide-react';

import { cn } from '@/components/ui/utils';
import type { DropTargetProps } from '@/lib/panes/pointer-drag';

// Unified tab rail. The pane strip, the expanded dock strip, and the studio
// right-rail all render through `TabStrip` + `Tab`; the collapsed dock uses
// `TabRail` (vertical). One component owns the things the three hand-rolled
// strips had drifted on:
//   • the `role="tablist"`/`role="tab"`/`aria-selected` + roving-tabindex model
//   • keyboard nav — ←/→ (or ↑/↓ vertical) switch, Home/End, and ⌃⇧←/→ reorder
//     when `onReorder` is supplied (the non-mouse alternative to drag-reorder)
//   • real keyboard-operable pin/close `<button>`s (replacing the old
//     `role="button" tabIndex={-1}` spans that nested invalidly inside a tab
//     `<button>` and could not be reached without a mouse) — Delete/Backspace
//     closes, `p` toggles pin on the focused tab
//   • the `.ikenga-tab-strip` styling contract: `data-ws` → `--tab-ws`, the
//     shared `[data-active]::after` underline (`var(--tab-ws, var(--primary))`,
//     no hardcoded amber), and `data-tabstrip-mixed` inactive hairlines
// Per-call-site look (padding, min-width, glyph, inline tint) stays at the call
// site — the structure + a11y + keyboard, which is where they had drifted, is
// what consolidates here.
//
// Dragging is pointer-event based (`lib/panes/pointer-drag.ts`), not HTML5
// DnD: tabs expose `onDragPointerDown` and strips accept `dropTarget` props.
//
// Spec: plans/shell-design-system/parts/components/tab-strip.md §3–§4
//       + designs/tab-strip.html (the locked Dusk Wood mockup).

type Orientation = 'horizontal' | 'vertical';

interface RovingArgs {
	orientation: Orientation;
	count: number;
	onSwitch: (idx: number) => void;
	onReorder?: (from: number, to: number) => void;
}

// Shared tablist keyboard handler. Reads the focused tab's `data-tab-index`,
// computes the neighbour, switches (or reorders with ⌃⇧) and moves DOM focus
// to the destination tab on the next frame (after React commits the reorder).
function useRovingTablist({ orientation, count, onSwitch, onReorder }: RovingArgs) {
	const containerRef = useRef<HTMLDivElement | null>(null);

	const focusTab = useCallback((i: number) => {
		requestAnimationFrame(() => {
			containerRef.current
				?.querySelector<HTMLElement>(`[role="tab"][data-tab-index="${i}"]`)
				?.focus();
		});
	}, []);

	const onKeyDown = useCallback(
		(e: React.KeyboardEvent<HTMLDivElement>) => {
			const tabEl = (e.target as HTMLElement).closest<HTMLElement>('[role="tab"]');
			if (!tabEl || !containerRef.current?.contains(tabEl)) return;
			const idx = Number(tabEl.dataset.tabIndex);
			if (Number.isNaN(idx)) return;

			const prevKey = orientation === 'vertical' ? 'ArrowUp' : 'ArrowLeft';
			const nextKey = orientation === 'vertical' ? 'ArrowDown' : 'ArrowRight';

			if (e.key === prevKey || e.key === nextKey) {
				const dir = e.key === nextKey ? 1 : -1;
				const to = idx + dir;
				if (to < 0 || to >= count) return;
				e.preventDefault();
				// ⌃⇧ (or ⌘⇧) + arrow = move the focused tab, the keyboard
				// alternative to drag-reorder (WCAG 2.5.7). Only when reorder is wired.
				if ((e.ctrlKey || e.metaKey) && e.shiftKey) {
					if (!onReorder) return;
					onReorder(idx, to);
				} else {
					onSwitch(to);
				}
				focusTab(to);
			} else if (e.key === 'Home') {
				e.preventDefault();
				onSwitch(0);
				focusTab(0);
			} else if (e.key === 'End') {
				e.preventDefault();
				onSwitch(count - 1);
				focusTab(count - 1);
			}
		},
		[orientation, count, onSwitch, onReorder, focusTab]
	);

	return { containerRef, onKeyDown };
}

export interface TabStripProps {
	/** Required tablist label (e.g. "Open tabs", "Dock tabs"). */
	label: string;
	/** Active tab index — drives the auto-scroll-into-view. */
	activeIdx: number;
	/** Total tab count — bounds keyboard navigation. */
	count: number;
	/** Switch to a tab (keyboard ←/→/Home/End). */
	onSwitch: (idx: number) => void;
	/** Reorder a tab (keyboard ⌃⇧←/→). Omit to disable reorder (dock/rail). */
	onReorder?: (from: number, to: number) => void;
	/** Mixed-workspace strip → per-tab inactive hairlines. */
	mixed?: boolean;
	className?: string;
	/** Extra inline style merged onto the scroller (e.g. drop-hover bg). */
	style?: React.CSSProperties;
	/** Pointer-drag drop target for the whole strip (`useDropTarget`). */
	dropTarget?: DropTargetProps;
	children: ReactNode;
}

/** Horizontal tab rail (pane / dock-expanded / studio-rail). */
export function TabStrip({
	label,
	activeIdx,
	count,
	onSwitch,
	onReorder,
	mixed,
	className,
	style,
	dropTarget,
	children,
}: TabStripProps) {
	const { containerRef, onKeyDown } = useRovingTablist({
		orientation: 'horizontal',
		count,
		onSwitch,
		onReorder,
	});

	// Scroll the active tab into view on activeIdx change (dedup-switch,
	// programmatic switch, reorder, new-tab add). We drive scrollLeft directly
	// rather than scrollIntoView — the latter's `block:'nearest'` can cascade
	// and scroll ancestor containers. Reduced-motion → instant.
	// biome-ignore lint/correctness/useExhaustiveDependencies: re-runs on activeIdx/count (the trigger), reads the stable scroller ref — by design.
	useEffect(() => {
		const scroller = containerRef.current;
		const tab = scroller?.querySelector<HTMLElement>('[role="tab"][data-active="true"]');
		if (!scroller || !tab) return;
		const tabLeft = tab.offsetLeft;
		const tabRight = tabLeft + tab.offsetWidth;
		const viewLeft = scroller.scrollLeft;
		const viewRight = viewLeft + scroller.clientWidth;
		let next = viewLeft;
		if (tabLeft < viewLeft) next = tabLeft;
		else if (tabRight > viewRight) next = tabRight - scroller.clientWidth;
		else return;
		const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
		scroller.scrollTo({ left: next, behavior: reduce ? 'auto' : 'smooth' });
	}, [activeIdx, count]);

	return (
		<div
			ref={containerRef}
			role="tablist"
			aria-label={label}
			onKeyDown={onKeyDown}
			className={cn(
				'ikenga-tab-strip flex items-stretch overflow-x-auto',
				'[&::-webkit-scrollbar]:hidden',
				className
			)}
			style={{ scrollbarWidth: 'none', ...style }}
			data-tabstrip-mixed={mixed ? 'true' : 'false'}
			{...dropTarget}
		>
			{children}
		</div>
	);
}

export interface TabProps {
	/** Position in the strip — drives roving focus + keyboard reorder math. */
	index: number;
	active: boolean;
	label: ReactNode;
	/** Workspace key → the `--tab-ws` tint (and the active-underline colour). */
	ws?: string;
	/** Leading view glyph (dock/rail). Pane passes none. */
	glyph?: ReactNode;
	title?: string;
	pinned?: boolean;
	/** Show + wire the close button (and Delete/Backspace). */
	closable?: boolean;
	/** Activate (switch to) this tab. */
	onActivate: () => void;
	onClose?: () => void;
	/** Wire the pin-toggle button (and the `p` key). */
	onTogglePin?: () => void;
	/** Middle-click on the tab body (pane/dock close-on-middle-click). */
	onMiddleClick?: () => void;
	/** Pointer-down on the tab body (not its pin/close buttons) — call
	 *  `beginPointerDrag` here to make the tab draggable. Omit = not draggable. */
	onDragPointerDown?: (e: React.PointerEvent<HTMLDivElement>) => void;
	/** Reorder drop indicator edge. */
	dropEdge?: 'before' | 'after' | null;
	/** Look preset: `rail` = uppercase mono studio tabs. */
	variant?: 'default' | 'rail';
	/** Extra classes (padding, min/max-width, border-r). */
	className?: string;
	/** Extra label classes (e.g. `capitalize`). */
	labelClassName?: string;
	style?: React.CSSProperties;
}

const AFFORDANCE = cn(
	'grid size-6 shrink-0 place-items-center rounded-sm',
	'opacity-0 transition-opacity group-hover:opacity-100 group-focus-within:opacity-100',
	'hover:bg-muted focus-visible:opacity-100 focus-visible:outline-none',
	'focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset',
	'motion-reduce:transition-none'
);

/** One tab. A `<div role="tab">` so the pin/close affordances can be real,
 *  keyboard-operable `<button>`s (a `<button>` can't nest in a `<button>`). */
export const Tab = forwardRef<HTMLDivElement, TabProps & HTMLAttributes<HTMLDivElement>>(
	function Tab(
		{
			index,
			active,
			label,
			ws,
			glyph,
			title,
			pinned = false,
			closable = false,
			onActivate,
			onClose,
			onTogglePin,
			onMiddleClick,
			onDragPointerDown,
			dropEdge,
			variant = 'default',
			className,
			labelClassName,
			style,
			// `...rest` carries the props a Radix <ContextMenuTrigger asChild>
			// injects (onContextMenu / onPointerDown / aria-*); spread first so
			// the explicit handlers below still win.
			...rest
		},
		ref
	) {
		return (
			<div
				ref={ref}
				{...rest}
				role="tab"
				aria-selected={active}
				tabIndex={active ? 0 : -1}
				data-tab-index={index}
				data-ws={ws}
				data-active={active ? 'true' : 'false'}
				title={title}
				onClick={onActivate}
				onPointerDown={(e) => {
					// Compose, don't replace: Radix's trigger listens here too.
					rest.onPointerDown?.(e);
					if (!onDragPointerDown || e.defaultPrevented) return;
					// The pin/close buttons are clicks, never drag handles.
					const nested = (e.target as Element).closest('button');
					if (nested && e.currentTarget.contains(nested)) return;
					onDragPointerDown(e);
				}}
				onAuxClick={(e) => {
					if (e.button === 1 && onMiddleClick) {
						e.preventDefault();
						onMiddleClick();
					}
				}}
				onKeyDown={(e) => {
					if (e.key === 'Enter' || e.key === ' ') {
						e.preventDefault();
						onActivate();
					} else if ((e.key === 'Delete' || e.key === 'Backspace') && closable && onClose) {
						e.preventDefault();
						onClose();
					} else if ((e.key === 'p' || e.key === 'P') && onTogglePin) {
						e.preventDefault();
						onTogglePin();
					}
				}}
				className={cn(
					'group relative flex shrink-0 cursor-default select-none items-center gap-2',
					'outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset',
					'motion-reduce:transition-none',
					variant === 'rail' ? 'text-[10px] uppercase tracking-wider' : 'text-xs',
					active
						? 'bg-background text-foreground'
						: 'text-muted-foreground hover:bg-accent hover:text-accent-foreground',
					className
				)}
				style={style}
			>
				{/* Pane pins show a leading rotated-pin indicator; glyph variants
			    (dock/rail) signal pinned state via the persistent pin button. */}
				{pinned && !glyph && <Pin className="h-3 w-3 shrink-0 -rotate-45 text-foreground/70" />}
				{glyph}
				<span className={cn('truncate', variant === 'rail' && 'font-mono', labelClassName)}>
					{label}
				</span>
				{onTogglePin && (
					<button
						type="button"
						tabIndex={-1}
						aria-label={pinned ? 'Unpin tab' : 'Pin tab'}
						onClick={(e) => {
							e.stopPropagation();
							onTogglePin();
						}}
						className={cn(AFFORDANCE, (pinned || active) && 'opacity-100')}
					>
						{pinned ? <PinOff className="h-3 w-3" /> : <Pin className="h-3 w-3" />}
					</button>
				)}
				{closable && onClose && (
					<button
						type="button"
						tabIndex={-1}
						aria-label="Close tab"
						onClick={(e) => {
							e.stopPropagation();
							onClose();
						}}
						className={cn(AFFORDANCE, active && 'opacity-60')}
					>
						<X className="h-3 w-3" />
					</button>
				)}
				{dropEdge && (
					<span
						aria-hidden="true"
						className={cn(
							'pointer-events-none absolute top-0 z-10 h-full w-0.5 bg-primary',
							dropEdge === 'before' ? 'left-0' : 'right-0'
						)}
					/>
				)}
			</div>
		);
	}
);

export interface TabRailProps {
	label: string;
	activeIdx: number;
	count: number;
	onSwitch: (idx: number) => void;
	className?: string;
	/** Pointer-drag drop target for the whole rail (`useDropTarget`). */
	dropTarget?: DropTargetProps;
	children: ReactNode;
}

/** Vertical icon rail for the collapsed dock. Shares the roving keyboard model
 *  (↑/↓ switch); tabs are `RailTab`s (icon-only, right-edge active marker). */
export function TabRail({ label, count, onSwitch, className, dropTarget, children }: TabRailProps) {
	const { containerRef, onKeyDown } = useRovingTablist({
		orientation: 'vertical',
		count,
		onSwitch,
	});
	return (
		<div
			ref={containerRef}
			role="tablist"
			aria-orientation="vertical"
			aria-label={label}
			onKeyDown={onKeyDown}
			className={cn('flex flex-col items-center gap-1', className)}
			{...dropTarget}
		>
			{children}
		</div>
	);
}

export interface RailTabProps {
	index: number;
	active: boolean;
	label: string;
	glyph: ReactNode;
	ws?: string;
	onActivate: () => void;
	/** Pointer-down on the rail tab — call `beginPointerDrag` to make it
	 *  draggable. Omit = not draggable. */
	onDragPointerDown?: (e: React.PointerEvent<HTMLButtonElement>) => void;
}

/** A single collapsed-dock rail tab (icon button + workspace-tinted edge marker). */
export function RailTab({
	index,
	active,
	label,
	glyph,
	ws,
	onActivate,
	onDragPointerDown,
}: RailTabProps) {
	return (
		<button
			type="button"
			role="tab"
			aria-selected={active}
			aria-label={label}
			title={label}
			tabIndex={active ? 0 : -1}
			data-tab-index={index}
			onClick={onActivate}
			onPointerDown={onDragPointerDown}
			className={cn(
				'relative grid size-7 place-items-center rounded-sm transition-colors hover:bg-card',
				'outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset',
				'motion-reduce:transition-none'
			)}
			style={{ color: active && ws ? `var(--tint-${ws}-fg)` : 'var(--fg-faint)' }}
		>
			{active && ws && (
				<span
					aria-hidden="true"
					className="absolute -right-1 top-1.5 bottom-1.5 w-0.5 rounded-l"
					style={{ background: `var(--tint-${ws}-fg)` }}
				/>
			)}
			{glyph}
		</button>
	);
}
