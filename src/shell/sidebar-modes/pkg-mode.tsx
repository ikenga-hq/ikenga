// Pkg sidebar — renders the runtime menu published by a pkg iframe via
// `host.pkg.setMenu`. Active when the focused pane is on a `/pkg/<id>/...`
// route. Clicks update the pkg-menu-store's active feature, which the
// pkg-iframe-host re-emits as a hostContext change so the iframe can swap
// its mounted view.

import {
	Activity,
	AlertTriangle,
	CalendarDays,
	CheckCheck,
	Folder,
	FolderKanban,
	LayoutDashboard,
	ListChecks,
	Mail,
	Package,
	Pencil,
	RefreshCw,
	Send,
	Sun,
	TrendingUp,
	type LucideIcon,
} from 'lucide-react';

import { Button } from '@/components/ui/button';
import { usePkgActivityBarEntries } from '@/lib/pkg/use-activity-bar-entries';
import { pkgSupervisorRestart } from '@/lib/tauri-cmd';

import { useState } from 'react';
import { Segmented } from '@/components/ui/segmented';
import { usePkgMenuStore, type PkgMenuItem } from '@/lib/pkg/pkg-menu-store';
import { SidebarNav, SidebarNavHeader, SidebarNavRow, SidebarNavSection } from './_nav';

// Stable empty-array sentinel — used by the menu selector when no menu has
// been published for this pkg yet. Without this, `?? []` returns a fresh
// array every render, Zustand sees a referential change, and the
// "getSnapshot should be cached" guard triggers a re-render loop.
const EMPTY_ITEMS: readonly PkgMenuItem[] = [];

const ICONS: Record<string, LucideIcon> = {
	'layout-dashboard': LayoutDashboard,
	'list-checks': ListChecks,
	'trending-up': TrendingUp,
	send: Send,
	mail: Mail,
	folder: Folder,
	'folder-kanban': FolderKanban,
	pencil: Pencil,
	package: Package,
	activity: Activity,
	'alert-triangle': AlertTriangle,
	'calendar-days': CalendarDays,
	'check-check': CheckCheck,
	sun: Sun,
};

function iconFor(name: string | null | undefined): LucideIcon {
	if (name && name in ICONS) return ICONS[name]!;
	return Package;
}

export function PkgMode({ pkgId }: { pkgId: string }) {
	const items = usePkgMenuStore((s) => s.menus[pkgId] ?? EMPTY_ITEMS);
	const activeFeature = usePkgMenuStore((s) => s.activeFeatures[pkgId]);
	const setActiveFeature = usePkgMenuStore((s) => s.setActiveFeature);
	const { entries } = usePkgActivityBarEntries();
	const entry = entries.find((e) => e.pkg_id === pkgId);

	if (items.length === 0) {
		return (
			<div className="h-full overflow-y-auto py-3 px-4 text-xs text-muted-foreground">
				{entry?.parked ? (
					<ParkedBanner pkgId={pkgId} reason={entry.parked_reason} />
				) : (
					'Waiting for pkg menu…'
				)}
			</div>
		);
	}

	// Group items by `section`. Adjacent items sharing the same section render
	// together under one heading; items with no section (or `null`) form the
	// implicit first group. Order is preserved as published.
	const groups: { section: string | null; items: PkgMenuItem[] }[] = [];
	for (const it of items) {
		const section = it.section ?? null;
		const last = groups[groups.length - 1];
		if (last && last.section === section) last.items.push(it);
		else groups.push({ section, items: [it] });
	}

	return (
		<SidebarNav ariaLabel="Package menu">
			{groups.map((g, gi) => (
				<SidebarNavSection
					key={g.section ?? `__implicit_${gi}`}
					label={g.section ?? undefined}
					className={gi > 0 ? 'border-t border-border/40 pt-3' : undefined}
				>
					{g.items.map((item) => {
						// Context header — publishing `subtitle` (even as `null`) opts the
						// item out of the row vocabulary entirely: no click, no dim, no
						// active state, whatever `disabled` / `active` say.
						if (item.subtitle !== undefined) {
							return <SidebarNavHeader key={item.id} label={item.label} subtitle={item.subtitle} />;
						}
						// Segmented view-switcher (the locked `list-kanban-switch`
						// pattern): renders as an inline pill strip; clicking an option
						// publishes the OPTION's id as the active feature.
						if (item.kind === 'seg' && item.options && item.options.length > 0) {
							const value =
								item.options.find((o) => o.active)?.id ??
								item.options.find((o) => o.id === activeFeature)?.id ??
								item.options[0]!.id;
							return (
								<Segmented
									key={item.id}
									variant="pill"
									ariaLabel={g.section ? `${g.section} mode` : 'View mode'}
									className="mx-2 my-1"
									items={item.options.map((o) => ({ id: o.id, label: o.label }))}
									value={value}
									onValueChange={(id) => {
										if (!item.disabled) setActiveFeature(pkgId, id);
									}}
								/>
							);
						}
						// Pkg-driven `active` wins; otherwise fall back to the store's
						// last-clicked feature. Disabled items never highlight / fire.
						const isActive = item.disabled ? false : (item.active ?? item.id === activeFeature);
						return (
							<SidebarNavRow
								key={item.id}
								icon={iconFor(item.icon)}
								label={item.label}
								active={isActive}
								disabled={item.disabled}
								badge={item.badge}
								onSelect={() => setActiveFeature(pkgId, item.id)}
							/>
						);
					})}
				</SidebarNavSection>
			))}
		</SidebarNav>
	);
}

/** Convenience: read the pkg ID from a route path. Returns null if not a pkg
 *  route. Re-exported here so AppMode doesn't need to import the store
 *  internals just to branch. */
export function pkgIdFromRoute(route: string | null | undefined): string | null {
	if (!route) return null;
	const m = route.match(/^\/pkg\/([^/]+)/);
	return m ? m[1]! : null;
}

function ParkedBanner({ pkgId, reason }: { pkgId: string; reason?: string | null }) {
	const [restarting, setRestarting] = useState(false);
	return (
		<div className="space-y-3">
			<div className="flex items-start gap-2 rounded-md border border-destructive/30 bg-destructive/10 p-3 text-xs">
				<AlertTriangle className="mt-0.5 h-4 w-4 shrink-0 text-destructive" />
				<div className="space-y-1">
					<p className="font-medium text-foreground">This package is parked</p>
					<p className="text-muted-foreground">
						{reason ?? 'The sidecar stopped after repeated crashes.'}
					</p>
				</div>
			</div>
			<Button
				size="sm"
				className="h-7 gap-1.5 text-xs"
				disabled={restarting}
				onClick={() => {
					setRestarting(true);
					void pkgSupervisorRestart(pkgId).finally(() => setRestarting(false));
				}}
			>
				<RefreshCw className="h-3 w-3" />
				Retry
			</Button>
		</div>
	);
}

export type { PkgMenuItem };
