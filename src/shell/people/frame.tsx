// D-05 People frame pieces shared by the People tabs (WP-72; WP-76 adds
// Members and Policies; WP-77 adds Audit).
//
// The design's `.phead` title + `new` chip, the tab strip (`.ptabs`), and the
// `.block` / `.srow2` settings rows (`designs/people.html`). Profile and
// Devices are personal; Members and Policies are per project; Audit is both
// (G-ACCESS §4, §11.1 "scope switch").

import { type ReactNode, useEffect, useState } from 'react';

import { SegmentedLinks } from '@/components/ui/segmented';
import { cn } from '@/components/ui/utils';
import { accessStatus } from '@/lib/access/client';
import { isT1Session } from '@/lib/transport/t1-session';

import { auditReadReason } from './audit-model';

export const PEOPLE_TABS = [
	{ to: '/settings/profile', label: 'Profile', exact: true },
	{ to: '/settings/devices', label: 'Devices', exact: true },
	{ to: '/settings/members', label: 'Members', exact: true },
	{ to: '/settings/policies', label: 'Policies', exact: true },
	{ to: '/settings/audit', label: 'Audit', exact: true },
] as const;

export type PeopleTab = 'profile' | 'devices' | 'members' | 'policies' | 'audit';
export type PeopleScope = 'personal' | 'project';

/** D-05 `TABS[].scopes`: the scopes a tab is live at (G-ACCESS §11.1). */
export const TAB_SCOPES: Readonly<Record<PeopleTab, readonly PeopleScope[]>> = {
	profile: ['personal'],
	devices: ['personal'],
	members: ['project'],
	policies: ['project'],
	// Audit: personal = rows you acted in or are about; project = the
	// active project's rows (`access_audit_list`'s `projectKey`).
	audit: ['personal', 'project'],
};

/** D-05 `#scopeSw`: the scope a tab opens at (G-ACCESS §11.1). */
export function tabScope(tab: PeopleTab): PeopleScope {
	return TAB_SCOPES[tab][0];
}

/** D-05 `TABS[].why`: why the other scope is disabled on a tab (as drawn).
 *  Audit has both scopes live, so no reason. */
export const TAB_SCOPE_WHY: Readonly<Partial<Record<PeopleTab, string>>> = {
	profile: 'A profile is yours, not the project’s.',
	devices: 'Devices pair to this machine, not to a project.',
	members: 'People are invited to a project, not to a machine.',
	policies: 'Roles are defined per project.',
};

const SCOPES = [
	{ id: 'personal', label: 'Personal' },
	{ id: 'project', label: 'Project' },
] as const;

/** D-05 `#scopeSw` (G-ACCESS §11.1 header controls): a tab that lives at
 *  one scope disables the other button with the tab's reason; on Audit both
 *  are live and `scope` / `onScope` drive them. */
export function PeopleScopeSwitch({
	tab,
	scope: chosen,
	onScope,
}: {
	tab: PeopleTab;
	scope?: PeopleScope;
	onScope?: (scope: PeopleScope) => void;
}) {
	const live = TAB_SCOPES[tab];
	const scope = chosen && live.includes(chosen) ? chosen : tabScope(tab);
	return (
		<fieldset
			id="scopeSw"
			aria-label="Scope"
			data-scope={scope}
			className="m-0 inline-flex min-w-0 overflow-hidden rounded-[var(--radius-sm,4px)] border border-[var(--border)] p-0"
		>
			{SCOPES.map((s) => {
				const on = s.id === scope;
				const enabled = live.includes(s.id) && (on || onScope !== undefined);
				return (
					<button
						key={s.id}
						type="button"
						data-scope={s.id}
						aria-pressed={on}
						disabled={!enabled}
						title={enabled ? undefined : TAB_SCOPE_WHY[tab]}
						onClick={enabled && !on ? () => onScope?.(s.id) : undefined}
						className={cn(
							'min-h-[26px] border-r border-[var(--border)] px-3 text-[var(--text-micro)] last:border-r-0',
							on
								? 'bg-[var(--primary-soft)] text-[var(--fg)]'
								: enabled
									? 'text-[var(--fg-muted)] hover:bg-[var(--bg-raised)] hover:text-[var(--fg)]'
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

/** G-ACCESS §6.7: why this credential can't open the Audit tab (a phone
 *  below `full`, or inside a share), or `null`. `known` skips the fetch
 *  when the caller already has the status (`undefined` = fetch it). An
 *  unreachable store leaves the tab enabled: the view explains that. */
function useAuditTabWhy(known: string | null | undefined): string | null {
	const [fetched, setFetched] = useState<string | null>(null);
	useEffect(() => {
		if (known !== undefined) return;
		let live = true;
		accessStatus()
			.then((s) => live && setFetched(auditReadReason(s)))
			.catch(() => live && setFetched(null));
		return () => {
			live = false;
		};
	}, [known]);
	return known !== undefined ? known : fetched;
}

/** The People tab strip. §6.7: phones below `full` can't open the Audit
 *  tab, and it is disabled with that reason (review m-4); the Audit view
 *  keeps the same reason for a direct URL. */
function PeopleTabs({ auditWhy }: { auditWhy: string | null }) {
	if (!auditWhy) return <SegmentedLinks items={[...PEOPLE_TABS]} ariaLabel="People sections" />;
	const audit = PEOPLE_TABS.find((t) => t.to === '/settings/audit');
	return (
		<div className="flex min-w-0 items-center gap-1">
			<SegmentedLinks items={PEOPLE_TABS.filter((t) => t !== audit)} ariaLabel="People sections" />
			<button
				type="button"
				disabled
				data-tab-disabled="audit"
				title={auditWhy}
				className="cursor-not-allowed whitespace-nowrap rounded-md px-3 py-1.5 text-xs font-medium text-muted-foreground opacity-45"
			>
				{audit?.label ?? 'Audit'}
			</button>
		</div>
	);
}

export function PeopleHeader({
	tab,
	scope,
	onScope,
	auditWhy: knownAuditWhy,
}: {
	tab: PeopleTab;
	scope?: PeopleScope;
	onScope?: (scope: PeopleScope) => void;
	/** The Audit tab's §6.7 reason when the caller knows the status. */
	auditWhy?: string | null;
}) {
	const auditWhy = useAuditTabWhy(knownAuditWhy);
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
					<PeopleScopeSwitch tab={tab} scope={scope} onScope={onScope} />
				</span>
			</header>
			<div className="flex items-center gap-2 border-b border-[var(--border-soft)] pb-2">
				<PeopleTabs auditWhy={auditWhy} />
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
