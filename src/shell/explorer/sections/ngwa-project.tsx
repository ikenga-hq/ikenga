import { useCallback } from 'react';
import { Package } from 'lucide-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import { pkgKernelStatus, pkgSetEnabled, pkgUninstall, type PkgInstalledSummary } from '@/lib/tauri-cmd';
import { itemDetailPath } from '@/lib/pkg/pkg-view-state';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import type { ExplorerSectionContext } from '../section-registry';

// WP-04 stub array — real menu content is `getEffectiveMenu('ngwa-project')`
// below (G-ACTIONS §1.3). Kept for `section-registry.ts`'s unused
// `contextMenu` field (out of this WP's FILES list; see the PR report).
export const ngwaProjectContextMenu = [
	{ id: 'open-detail', label: 'Open detail', run: () => {} },
	{ id: 'open-definition', label: 'Open definition file', run: () => {} },
	{ id: 'change-scope', label: 'Change scope…', run: () => {} },
	{ id: 'disable', label: 'Disable', run: () => {} },
	{ id: 'uninstall', label: 'Uninstall…', run: () => {} },
];

export function NgwaProjectSection({ projectId }: ExplorerSectionContext) {
	const query = useQuery<PkgInstalledSummary[]>({
		queryKey: ['explorer-ngwa-project', projectId],
		queryFn: async () => {
			try {
				const status = await pkgKernelStatus();
				return status.installed.filter(
					(p) => !p.project_id || p.project_id === projectId
				);
			} catch {
				return [];
			}
		},
		staleTime: 30_000,
	});

	const pkgs = query.data ?? [];

	const openPkg = useCallback((pkgId: string) => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: `/pkg/${pkgId}` });
	}, []);

	const openDetail = useCallback((pkgId: string) => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: itemDetailPath(pkgId) });
	}, []);

	const openStore = useCallback(() => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: '/ngwa/store' });
	}, []);

	if (pkgs.length === 0) {
		return (
			<div className="p-4 text-center">
				<h3 className="text-sm font-semibold">No packages installed</h3>
				<p className="text-xs text-muted-foreground mt-1 mb-3">
					Packages, skills, agents and tools are installed from the Ngwa Store.
				</p>
				<button
					type="button"
					onClick={openStore}
					className="text-xs bg-primary text-primary-foreground px-3 py-1.5 rounded hover:bg-primary/90 transition-colors"
				>
					Browse Store
				</button>
			</div>
		);
	}

	const qc = useQueryClient();

	return (
		<div className="py-1">
			{pkgs.map((pkg) => (
				<EffectiveContextMenu
					key={pkg.id}
					menuId="ngwa-project"
					handlers={{
						'open-detail': () => openDetail(pkg.id),
						// No standalone definition-file viewer exists yet — the item
						// detail page is the closest surface that shows it.
						'open-definition': () => openDetail(pkg.id),
						// No scope-change endpoint exists yet — same stand-in.
						'change-scope': () => openDetail(pkg.id),
						disable: () => {
							void pkgSetEnabled(pkg.id, false).then(() =>
								qc.invalidateQueries({ queryKey: ['explorer-ngwa-project', projectId] })
							);
						},
						uninstall: () => {
							if (!window.confirm(`Uninstall "${pkg.id}"?`)) return;
							void pkgUninstall(pkg.id).then(() =>
								qc.invalidateQueries({ queryKey: ['explorer-ngwa-project', projectId] })
							);
						},
					}}
				>
					<ListRow
						size="sm"
						onActivate={() => openPkg(pkg.id)}
						title={pkg.id}
						className="w-full gap-1.5 px-2"
					>
						<Package className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
						<span className="flex-1 truncate text-xs">{pkg.id}</span>
						<span className="text-[10px] text-muted-foreground font-mono">v{pkg.version}</span>
					</ListRow>
				</EffectiveContextMenu>
			))}
		</div>
	);
}
