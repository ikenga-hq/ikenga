// D-05 People frame pieces shared by the People tabs (WP-72; WP-76 adds
// Members and Policies).
//
// The design's `.phead` title + `new` chip, the tab strip (`.ptabs`), and the
// `.block` / `.srow2` settings rows (`designs/people.html`). Profile and
// Devices are personal; Members and Policies are per project (G-ACCESS §4,
// §11.1 "scope switch"). Audit comes with WP-77.

import type { ReactNode } from 'react';

import { SegmentedLinks } from '@/components/ui/segmented';
import { cn } from '@/components/ui/utils';
import { isT1Session } from '@/lib/transport/t1-session';

export const PEOPLE_TABS = [
	{ to: '/settings/profile', label: 'Profile', exact: true },
	{ to: '/settings/devices', label: 'Devices', exact: true },
	{ to: '/settings/members', label: 'Members', exact: true },
	{ to: '/settings/policies', label: 'Policies', exact: true },
] as const;

export type PeopleTab = 'profile' | 'devices' | 'members' | 'policies';

/** D-05 `#scopeSw`: which scope a tab lives at (G-ACCESS §11.1). */
export function tabScope(tab: PeopleTab): 'personal' | 'project' {
	return tab === 'members' || tab === 'policies' ? 'project' : 'personal';
}

/** D-05 `TABS[].why`: why the other scope is disabled on a tab (as drawn). */
export const TAB_SCOPE_WHY: Record<PeopleTab, string> = {
	profile: 'A profile is yours, not the project’s.',
	devices: 'Devices pair to this machine, not to a project.',
	members: 'People are invited to a project, not to a machine.',
	policies: 'Roles are defined per project.',
};

const SCOPES = [
	{ id: 'personal', label: 'Personal' },
	{ id: 'project', label: 'Project' },
] as const;

/** D-05 `#scopeSw` (G-ACCESS §11.1 header controls): each People tab lives
 *  at one scope, so the other button is disabled with the tab's reason. */
export function PeopleScopeSwitch({ tab }: { tab: PeopleTab }) {
	const scope = tabScope(tab);
	return (
		<fieldset
			id="scopeSw"
			aria-label="Scope"
			data-scope={scope}
			className="m-0 inline-flex min-w-0 overflow-hidden rounded-[var(--radius-sm,4px)] border border-[var(--border)] p-0"
		>
			{SCOPES.map((s) => {
				const on = s.id === scope;
				return (
					<button
						key={s.id}
						type="button"
						data-scope={s.id}
						aria-pressed={on}
						disabled={!on}
						title={on ? undefined : TAB_SCOPE_WHY[tab]}
						className={cn(
							'min-h-[26px] border-r border-[var(--border)] px-3 text-[var(--text-micro)] last:border-r-0',
							on
								? 'bg-[var(--primary-soft)] text-[var(--fg)]'
								: 'cursor-not-allowed text-[var(--fg-muted)] opacity-45'
						)}
					>
						{s.label}
					</button>
				);
			})}
		</fieldset>
	);
}

/** D-4 (G-ACCESS §11.2): where access data lives. T1 → the server operator
 *  database; T0 → the daemon's `<data-dir>/access.db` (the desktop proxies
 *  `access_*` to the daemon, P-20, so the window can't name that dir). */
export const T1_ACCESS_STORE = 'server operator database';
export const T0_ACCESS_STORE = '<data-dir>/access.db';

export function accessStorePath(t1: boolean): string {
	return t1 ? T1_ACCESS_STORE : T0_ACCESS_STORE;
}

/** D-05 `.filebar` with D-4 applied: `access store · <path>`. No "Open
 *  file" button (there is no file a principal may open) and no iyke line
 *  (D-10, §15 N-5). */
export function PeopleFileBar({ t1 = isT1Session() }: { t1?: boolean }) {
	return (
		<div
			data-filebar="access-store"
			className="flex items-center gap-2 border-t border-[var(--border-soft)] pt-2 font-mono text-[var(--text-micro)]"
		>
			<span className="text-[var(--fg-faint,var(--fg-muted))]">writes</span>
			<span className="truncate text-[var(--fg-muted)]">access store · {accessStorePath(t1)}</span>
		</div>
	);
}

export function PeopleHeader({ tab }: { tab: PeopleTab }) {
	return (
		<div className="space-y-3">
			<header className="flex flex-wrap items-center gap-2">
				<h2
					className="text-2xl font-semibold tracking-tight"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					People, devices and access
				</h2>
				<span className="ml-auto">
					<PeopleScopeSwitch tab={tab} />
				</span>
			</header>
			<div className="flex items-center gap-2 border-b border-[var(--border-soft)] pb-2">
				<SegmentedLinks items={[...PEOPLE_TABS]} ariaLabel="People sections" />
				<span className="ml-auto font-mono text-[11px] text-[var(--fg-muted)]">
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
