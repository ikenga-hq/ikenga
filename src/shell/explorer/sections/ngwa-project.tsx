import { useCallback } from 'react';
import { Package } from 'lucide-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import type { NgwaItem } from '@ikenga/contract';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import {
	pkgKernelStatus,
	pkgSetEnabled,
	pkgUninstall,
	type PkgInstalledSummary,
} from '@/lib/tauri-cmd';
import { itemDetailPath } from '@/lib/pkg/pkg-view-state';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import { useNgwaSnapshot, ngwaSnapshotQueryKey } from '@/lib/ngwa/use-ngwa-snapshot';
import { selectProjectNgwaItems } from '@/lib/ngwa/select-project-items';
import { isPkgItem } from '@/shell/ngwa/ngwa-scope-model';
import { kindIcon } from '@/lib/ngwa/kind-icon';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import type { ExplorerSectionContext } from '../section-registry';

const FALLBACK_QUERY_KEY_PREFIX = 'explorer-ngwa-project-fallback';

/** The kernel pkg rows this section showed before DEC-74, kept as the
 *  loading-state fallback: the shared Ngwa snapshot (`use-ngwa-snapshot.ts`)
 *  can take ~70s on a cold scan, and the section must never go blank while
 *  it resolves. */
function useFallbackPkgs(projectId: string): PkgInstalledSummary[] {
	const query = useQuery<PkgInstalledSummary[]>({
		queryKey: [FALLBACK_QUERY_KEY_PREFIX, projectId],
		queryFn: async () => {
			try {
				const status = await pkgKernelStatus();
				return status.installed.filter((p) => !p.project_id || p.project_id === projectId);
			} catch {
				return [];
			}
		},
		staleTime: 30_000,
	});
	return query.data ?? [];
}

export type NgwaProjectRows =
	| { kind: 'snapshot'; items: NgwaItem[] }
	| { kind: 'fallback'; pkgs: PkgInstalledSummary[] };

/** DEC-74 rows: this project's Ngwa snapshot items (`selectProjectNgwaItems`)
 *  once the shared snapshot query has resolved at least once, else the
 *  kernel-pkg fallback above. Reads the existing shared snapshot query
 *  (`useNgwaSnapshot`) — never issues a second cold scan. Exported so the
 *  section-registry row-count badge (`useCount`) can't disagree with what
 *  actually renders. */
export function useNgwaProjectRows(projectId: string): NgwaProjectRows {
	const { items, isLoading } = useNgwaSnapshot();
	const fallbackPkgs = useFallbackPkgs(projectId);
	if (isLoading) return { kind: 'fallback', pkgs: fallbackPkgs };
	return { kind: 'snapshot', items: selectProjectNgwaItems(items, projectId) };
}

export function useNgwaProjectRowCount(projectId: string): number {
	const rows = useNgwaProjectRows(projectId);
	return rows.kind === 'snapshot' ? rows.items.length : rows.pkgs.length;
}

export function NgwaProjectSection({ projectId }: ExplorerSectionContext) {
	const rows = useNgwaProjectRows(projectId);
	const qc = useQueryClient();

	const openDetail = useCallback((id: string) => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: itemDetailPath(id) });
	}, []);

	const openStore = useCallback(() => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: '/ngwa/store' });
	}, []);

	const isEmpty = rows.kind === 'snapshot' ? rows.items.length === 0 : rows.pkgs.length === 0;

	if (isEmpty) {
		return (
			<div className="p-4 text-center">
				<h3 className="text-sm font-semibold">No apps or extensions installed</h3>
				<p className="text-xs text-muted-foreground mt-1 mb-3">
					Apps, skills, agents and tools are installed from the Ngwa Store.
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

	if (rows.kind === 'fallback') {
		return (
			<div className="py-1">
				{rows.pkgs.map((pkg) => (
					<EffectiveContextMenu
						key={pkg.id}
						menuId="ngwa-project"
						target={{ ngwaItemKind: undefined }}
						// A-9: `open-definition` and `change-scope` are left out — no
						// definition-file viewer or scope-change endpoint exists, and a
						// row that only opens the detail page is not that behaviour.
						builtinsNeedHandler
						handlers={{
							'open-detail': () => openDetail(pkg.id),
							disable: () => {
								void pkgSetEnabled(pkg.id, false).then(() =>
									qc.invalidateQueries({ queryKey: [FALLBACK_QUERY_KEY_PREFIX, projectId] })
								);
							},
							uninstall: () => {
								void (async () => {
									const ok = await confirmDialog(`Uninstall "${pkg.id}"?`, {
										title: 'Uninstall',
										kind: 'warning',
									});
									if (!ok) return;
									await pkgUninstall(pkg.id);
									await qc.invalidateQueries({ queryKey: [FALLBACK_QUERY_KEY_PREFIX, projectId] });
								})().catch(() => {});
							},
						}}
					>
						<ListRow
							size="sm"
							onActivate={() => openDetail(pkg.id)}
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

	return (
		<div className="py-1">
			{rows.items.map((item) => {
				const row = (
					<ListRow
						size="sm"
						onActivate={() => openDetail(item.id)}
						title={item.display_name}
						className="w-full gap-1.5 px-2"
					>
						{kindIcon(item.kind)}
						<span className="flex-1 truncate text-xs">{item.display_name}</span>
						<span className="text-[10px] text-muted-foreground">{item.kind}</span>
						{isPkgItem(item) && item.version && (
							<span className="text-[10px] text-muted-foreground font-mono">v{item.version}</span>
						)}
					</ListRow>
				);

				// Only pkg rows keep the existing context menu (disable / uninstall) —
				// an Ọba / config-scan item (skill, agent, hook, mcp, workflow) has
				// neither action wired for it here.
				if (!isPkgItem(item)) {
					return <div key={item.id}>{row}</div>;
				}

				return (
					<EffectiveContextMenu
						key={item.id}
						menuId="ngwa-project"
						target={{ ngwaItemKind: item.kind }}
						builtinsNeedHandler
						handlers={{
							'open-detail': () => openDetail(item.id),
							disable: () => {
								void pkgSetEnabled(item.id, false).then(() =>
									qc.invalidateQueries({ queryKey: ngwaSnapshotQueryKey })
								);
							},
							uninstall: () => {
								void (async () => {
									const ok = await confirmDialog(`Uninstall "${item.id}"?`, {
										title: 'Uninstall',
										kind: 'warning',
									});
									if (!ok) return;
									await pkgUninstall(item.id);
									await qc.invalidateQueries({ queryKey: ngwaSnapshotQueryKey });
								})().catch(() => {});
							},
						}}
					>
						{row}
					</EffectiveContextMenu>
				);
			})}
		</div>
	);
}
