import { useCallback } from 'react';
import { LayoutGrid } from 'lucide-react';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import { usePkgActivityBarEntries, type PkgViewEntry } from '@/lib/pkg/use-activity-bar-entries';
import { usePinsStore } from '@/lib/shell/pins-store';
import { PinIcon } from '@/shell/pin-icon';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import type { ExplorerSectionContext } from '../section-registry';

// WP-04 stub array — real menu content is `getEffectiveMenu('views')` below
// (G-ACTIONS §1.3). Kept for `section-registry.ts`'s unused `contextMenu`
// field (out of this WP's FILES list; see the PR report).
export const viewsContextMenu = [
	{ id: 'open', label: 'Open', run: () => {} },
	{ id: 'open-side', label: 'Open to the Side', run: () => {} },
	{ id: 'pin-rail', label: 'Pin to rail', run: () => {} },
	{ id: 'open-ngwa', label: 'Open pkg in Ngwa', run: () => {} },
];

/** Explorer **Views** section — renders every `ui.views[]` contribution
 *  across installed pkgs (manifest v5, G-MANIFEST-V5 §2). The `views` list
 *  from `usePkgActivityBarEntries` is registry-canonical — the `ui.nav`
 *  alias was removed outright by DEC-37, so nothing here needs a legacy
 *  fallback. Each row opens the view's pane route
 *  (`/pkg/<id><route>`) in the focused pane. */
export function ViewsSection(_ctx: ExplorerSectionContext) {
	const { views, loaded } = usePkgActivityBarEntries();
	const addPin = usePinsStore((s) => s.addPin);

	const openView = useCallback((route: string) => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: route });
	}, []);

	const openViewSplit = useCallback((route: string) => {
		const { focusedId, placeView } = usePaneStore.getState();
		placeView(focusedId, { kind: 'route', path: route }, 'right');
	}, []);

	const pinToRail = useCallback(
		(view: PkgViewEntry) => {
			void addPin({
				kind: 'pkg-route',
				target: view.pane_route,
				label: view.title,
				iconLucide: view.icon ?? null,
			}).catch(() => {});
		},
		[addPin]
	);

	const openPkgInNgwa = useCallback((view: PkgViewEntry) => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: `/ngwa/item/${view.pkg_id}` });
	}, []);

	const openNgwa = useCallback(() => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: '/ngwa/installed' });
	}, []);

	if (loaded && views.length === 0) {
		return (
			<div className="p-4 text-center">
				<h3 className="text-sm font-semibold">No contributed views</h3>
				<p className="text-xs text-muted-foreground mt-1 mb-3">
					Packages contribute views to the Explorer.
				</p>
				<button
					type="button"
					onClick={openNgwa}
					className="text-xs bg-primary text-primary-foreground px-3 py-1.5 rounded hover:bg-primary/90 transition-colors"
				>
					Browse packages
				</button>
			</div>
		);
	}

	return (
		<div className="py-1">
			{views.map((view: PkgViewEntry) => (
				<EffectiveContextMenu
					key={view.qualified_id}
					menuId="views"
					handlers={{
						open: () => openView(view.pane_route),
						'open-to-side': () => openViewSplit(view.pane_route),
						'pin-rail': () => pinToRail(view),
						'open-in-ngwa': () => openPkgInNgwa(view),
					}}
				>
					<ListRow
						size="sm"
						onActivate={() => openView(view.pane_route)}
						title={view.title}
						className="w-full gap-1.5 px-2"
					>
						<PinIcon
							iconLucide={view.icon ?? null}
							iconEmoji={null}
							Fallback={LayoutGrid}
							sizeClass="h-3.5 w-3.5"
							className="shrink-0 text-muted-foreground"
						/>
						<span className="flex-1 truncate text-xs">{view.title}</span>
						<span className="shrink-0 truncate text-[10px] text-muted-foreground/70">
							{view.pkg_name}
						</span>
					</ListRow>
				</EffectiveContextMenu>
			))}
		</div>
	);
}
