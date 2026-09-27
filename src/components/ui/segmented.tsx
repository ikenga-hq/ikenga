import { useCallback, useRef, type ReactNode } from 'react';
import { Link, useRouterState } from '@tanstack/react-router';

import { cn } from '@/components/ui/utils';

// Segmented — the shared "pick one of N mutually-exclusive views" control.
// Consolidates the hand-rolled segmented-pill switchers the shell had drifted
// on. Two live shapes ship here:
//
//   • Segmented (variant "pill") — a compact route/in-surface tab-pill strip.
//     `role="tablist"` + roving ←/→/Home/End + `aria-selected`, the things the
//     hand-rolled SectionTabs/ModeChoice lacked.
//   • Segmented (variant "card") — the taller two-up dialog choice (NewSession
//     ModeChoice): icon + label + detail, `aria-pressed`, standard Tab flow.
//   • SegmentedLinks — the router-driven pill strip (the SectionTabs shape):
//     TanStack `<Link>`s carry navigation, so per the HTML spec they are a
//     `role="navigation"` list with `aria-current="page"`, NOT a tablist.
//
// Out of scope by design (documented in segmented-view-switcher.md §2/§4):
//   • the studio right-rail underline strip → already served by TabStrip
//     (tab-strip.md); it is a strip, not a segmented pill.
//   • the ngwa Surface/Scope/Kind sidebar lists → intentionally local to the
//     claude-config idiom and owned by the Ngwa design-system plan (vertical
//     facet-nav rows, not a horizontal segment).
//
// Spec: plans/shell-design-system/parts/components/segmented-view-switcher.md
//       + designs/segmented-view-switcher.html (the locked Dusk Wood mockup).
// Tokens only — no hardcoded amber; the active fill is `bg-accent`/`border-primary`
// and selection is always carried by ARIA, never colour alone (WCAG 1.4.1).

const FOCUS_RING =
	'outline-none focus-visible:outline-solid focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary';

export interface SegmentedItem {
	id: string;
	label: string;
	/** Pre-rendered leading glyph (pill: small; card: in the label row). */
	icon?: ReactNode;
	/** Card variant only — muted subtext line beneath the label. */
	detail?: string;
	/** Pill variant only — trailing mono count badge. */
	count?: number;
	disabled?: boolean;
}

export interface SegmentedProps {
	items: SegmentedItem[];
	value: string;
	onValueChange: (id: string) => void;
	variant?: 'pill' | 'card';
	/** Required group/tablist label (WCAG 4.1.2 + 1.3.1). */
	ariaLabel: string;
	className?: string;
}

/** Roving-tablist keyboard for the pill strip: ←/→ cycle enabled tabs, Home/End
 *  jump the ends, focus follows selection. Disabled items are skipped. */
function useRovingSegment(
	items: SegmentedItem[],
	value: string,
	onValueChange: (id: string) => void
) {
	const ref = useRef<HTMLDivElement | null>(null);

	const focusTab = useCallback((id: string) => {
		requestAnimationFrame(() => {
			ref.current?.querySelector<HTMLElement>(`[role="tab"][data-seg-id="${id}"]`)?.focus();
		});
	}, []);

	const enabled = items.filter((i) => !i.disabled);

	const onKeyDown = useCallback(
		(e: React.KeyboardEvent<HTMLDivElement>) => {
			if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
			if (enabled.length === 0) return;
			const pos = enabled.findIndex((i) => i.id === value);
			let next = pos;
			if (e.key === 'ArrowRight') next = pos < 0 ? 0 : Math.min(pos + 1, enabled.length - 1);
			else if (e.key === 'ArrowLeft') next = pos < 0 ? 0 : Math.max(pos - 1, 0);
			else if (e.key === 'Home') next = 0;
			else if (e.key === 'End') next = enabled.length - 1;
			const target = enabled[next];
			if (!target || target.id === value) {
				e.preventDefault();
				return;
			}
			e.preventDefault();
			onValueChange(target.id);
			focusTab(target.id);
		},
		[enabled, value, onValueChange, focusTab]
	);

	return { ref, onKeyDown };
}

export function Segmented({
	items,
	value,
	onValueChange,
	variant = 'pill',
	ariaLabel,
	className,
}: SegmentedProps) {
	const { ref, onKeyDown } = useRovingSegment(items, value, onValueChange);

	if (variant === 'card') {
		// Card grid: standard Tab order, aria-pressed (toggle semantics).
		return (
			<div role="group" aria-label={ariaLabel} className={cn('grid grid-cols-2 gap-2', className)}>
				{items.map((item) => {
					const active = item.id === value;
					return (
						<button
							key={item.id}
							type="button"
							aria-pressed={active}
							disabled={item.disabled}
							onClick={() => onValueChange(item.id)}
							className={cn(
								'flex flex-col items-start gap-1 rounded-md border px-3 py-2 text-left transition-colors motion-reduce:transition-none',
								FOCUS_RING,
								'disabled:pointer-events-none disabled:opacity-40',
								active
									? 'border-primary bg-primary/5'
									: 'border-input bg-background hover:bg-accent'
							)}
						>
							<span className="flex items-center gap-2 text-sm font-medium">
								{item.icon}
								{item.label}
							</span>
							{item.detail && (
								<span className="text-[11px] text-muted-foreground">{item.detail}</span>
							)}
						</button>
					);
				})}
			</div>
		);
	}

	// Pill strip: role=tablist + roving keyboard.
	return (
		<div
			ref={ref}
			role="tablist"
			aria-label={ariaLabel}
			onKeyDown={onKeyDown}
			className={cn('flex gap-1 overflow-x-auto', className)}
		>
			{items.map((item) => {
				const active = item.id === value;
				return (
					<button
						key={item.id}
						type="button"
						role="tab"
						aria-selected={active}
						data-seg-id={item.id}
						tabIndex={active ? 0 : -1}
						disabled={item.disabled}
						onClick={() => onValueChange(item.id)}
						className={cn(
							'flex items-center gap-1.5 whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition-colors motion-reduce:transition-none',
							FOCUS_RING,
							'disabled:pointer-events-none disabled:opacity-40',
							active
								? 'bg-accent text-foreground'
								: 'text-muted-foreground hover:bg-accent hover:text-foreground'
						)}
					>
						{item.icon}
						<span className="truncate">{item.label}</span>
						{typeof item.count === 'number' && (
							<span className="ml-0.5 font-mono text-[10px] text-muted-foreground">
								{item.count}
							</span>
						)}
					</button>
				);
			})}
		</div>
	);
}

export interface SegmentedLinkItem {
	to: string;
	label: string;
	/** When true, only active on an exact path match (keeps the index tab from
	 *  staying active on every sub-route). */
	exact?: boolean;
}

/** Router-driven pill strip — items are `<Link>`s, so it is a navigation list
 *  with `aria-current="page"` rather than a tablist (HTML semantics). Shares the
 *  pill visual vocabulary + focus ring + reduced-motion guard with `Segmented`. */
export function SegmentedLinks({
	items,
	ariaLabel = 'Section navigation',
	className,
}: {
	items: SegmentedLinkItem[];
	ariaLabel?: string;
	className?: string;
}) {
	const { location } = useRouterState();
	const path = location.pathname;
	return (
		<nav aria-label={ariaLabel} className={cn('flex gap-1 overflow-x-auto', className)}>
			{items.map((item) => {
				const isActive = item.exact ? path === item.to : path.startsWith(item.to);
				return (
					<Link
						key={item.to}
						to={item.to}
						aria-current={isActive ? 'page' : undefined}
						className={cn(
							'whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium transition-colors motion-reduce:transition-none',
							FOCUS_RING,
							'text-muted-foreground hover:bg-accent hover:text-foreground',
							isActive && 'bg-accent text-foreground'
						)}
					>
						{item.label}
					</Link>
				);
			})}
		</nav>
	);
}
