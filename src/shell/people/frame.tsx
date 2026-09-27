// D-05 People frame pieces shared by the Profile and Devices tabs (WP-72).
//
// The design's `.phead` title + `new` chip, the tab strip (`.ptabs`), and the
// `.block` / `.srow2` settings rows (`designs/people.html`). Only the tabs
// that don't need a principal exist yet: Profile and Devices. Members,
// Policies and Audit come with G-ACCESS (WP-76, WP-77). G-101 leaves them out
// on purpose rather than mocking them.

import type { ReactNode } from 'react';

import { SegmentedLinks } from '@/components/ui/segmented';
import { cn } from '@/components/ui/utils';

export const PEOPLE_TABS = [
	{ to: '/settings/profile', label: 'Profile', exact: true },
	{ to: '/settings/devices', label: 'Devices', exact: true },
] as const;

export function PeopleHeader({ tab }: { tab: 'profile' | 'devices' }) {
	return (
		<div className="space-y-3">
			<header className="flex flex-wrap items-center gap-2">
				<h2
					className="text-2xl font-semibold tracking-tight"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					People, devices and access
				</h2>
			</header>
			<div className="flex items-center gap-2 border-b border-[var(--border-soft)] pb-2">
				<SegmentedLinks items={[...PEOPLE_TABS]} ariaLabel="People sections" />
				<span className="ml-auto font-mono text-[10px] text-[var(--fg-muted)]">
					/settings/{tab}
				</span>
			</div>
		</div>
	);
}

/** D-05 `.block`: a bordered card with an uppercase micro heading. */
export function PeopleBlock({
	title,
	right,
	children,
	className,
}: {
	title: ReactNode;
	right?: ReactNode;
	children: ReactNode;
	className?: string;
}) {
	return (
		<section
			className={cn(
				'overflow-hidden rounded-[var(--radius-md,6px)] border border-[var(--border)] bg-[var(--bg-surface)]',
				className
			)}
		>
			<h3 className="m-0 flex h-[30px] items-center gap-2 border-b border-[var(--border-soft)] px-3 text-[var(--text-micro)] font-semibold uppercase tracking-[0.1em] text-[var(--fg-muted)]">
				{title}
				{right && (
					<span className="ml-auto flex items-center gap-2 font-normal normal-case tracking-normal">
						{right}
					</span>
				)}
			</h3>
			<div className="px-3">{children}</div>
		</section>
	);
}

/** D-05 `.srow2`: label (+ sub) on the left, control on the right. */
export function PeopleRow({
	label,
	sub,
	children,
	top,
	htmlFor,
}: {
	label: ReactNode;
	sub?: ReactNode;
	children: ReactNode;
	/** Align to the top (multi-line controls). */
	top?: boolean;
	/** Makes the label a <label> for this control id. */
	htmlFor?: string;
}) {
	return (
		<div
			className={cn(
				'flex min-h-8 flex-wrap gap-3 border-b border-[var(--border-soft)] py-2 last:border-b-0',
				top ? 'items-start' : 'items-center'
			)}
		>
			<span className="w-[176px] flex-none text-[var(--text-caption,12px)] text-[var(--fg)]">
				{htmlFor ? <label htmlFor={htmlFor}>{label}</label> : label}
				{sub && (
					<span className="mt-px block text-[var(--text-micro)] leading-snug text-[var(--fg-muted)]">
						{sub}
					</span>
				)}
			</span>
			<span className="flex min-w-0 flex-1 flex-wrap items-center gap-2">{children}</span>
		</div>
	);
}

/** D-05 `.kv`: a quiet mono value. */
export function Kv({ children, className }: { children: ReactNode; className?: string }) {
	return (
		<span className={cn('font-mono text-[var(--text-micro)] text-[var(--fg-muted)]', className)}>
			{children}
		</span>
	);
}
