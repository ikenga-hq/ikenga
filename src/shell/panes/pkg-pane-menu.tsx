// WP-45 — pkg branch of the pane `⋯` menu (D-08 `pkg-view`,
// `designs/pane-chrome.html?state=pkg-view`, `pkgDotsMenu()`).
//
// Design order: Reload view · Open devtools · View permissions · Package
// settings · Restart sidecar ─ Unpin · Report violation log. Every item is
// bound to an action that already exists, or omitted:
//
//   Reload view           → the pane's own refresh (`refreshPane` /
//                           the merged row's `onRefresh`), which re-runs the
//                           iframe host's fetch + handshake.
//   Open devtools         → OMITTED. The only devtools opener is the iyke
//                           HTTP bridge (`POST /iyke/devtools`, debug builds
//                           only, primary window); there is no Tauri command.
//   View permissions      → Ngwa item detail (`/ngwa/item/<pkgId>`) in a new
//   Package settings        tab of this pane. It owns both the Permissions and
//                           Settings tabs; it has no tab deep-link yet, so
//                           both land on Overview.
//   Restart sidecar       → `pkg_supervisor_restart` — shown only when the
//                           pkg has a supervised (long-lived) sidecar.
//   Unpin                 → `usePinsStore.removePin` — shown only when a
//                           rail pin targets this pkg.
//   Report violation log  → Ngwa Health violations panel
//                           (`/ngwa/health?section=violations`) in a new tab.

import { useQuery } from '@tanstack/react-query';
import { AlertTriangle, Bolt, PinOff, RefreshCw, Settings, Shield } from 'lucide-react';
import { DropdownMenuItem, DropdownMenuSeparator } from '@/components/ui/dropdown-menu';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneId, PaneView } from '@/lib/panes/types';
import {
	itemDetailPath,
	pkgIdFromRoutePath,
	VIOLATION_LOG_PATH,
} from '@/lib/pkg/pkg-view-state';
import { usePinsStore } from '@/lib/shell/pins-store';
import { pkgKernelStatus, pkgSupervisorRestart } from '@/lib/tauri-cmd';

interface SupervisorRegistry {
	entries?: Array<{ pkg_id: string }>;
}

/** The pkg a pane's active tab is showing, or null for non-pkg views. */
export function usePkgIdForPane(paneId: PaneId): string | null {
	return usePaneStore((s) => {
		const leaf = findLeaf(s.root, paneId);
		const view: PaneView | undefined = leaf?.tabs[leaf.activeTabIdx];
		return view?.kind === 'route' ? pkgIdFromRoutePath(view.path) : null;
	});
}

export function PkgPaneMenuItems({
	paneId,
	pkgId,
	onReload,
}: {
	paneId: PaneId;
	pkgId: string;
	onReload: () => void;
}) {
	const addTab = usePaneStore((s) => s.addTab);
	// Same key + staleness as `useWebviewRoute` (pane-views.tsx) so the pane
	// chrome shares one kernel-status read.
	const { data: status } = useQuery({
		queryKey: ['pkg-kernel-status'],
		queryFn: pkgKernelStatus,
		staleTime: Infinity,
	});
	const supervised = Boolean(
		(status?.registries?.sidecar_supervisor as SupervisorRegistry | undefined)?.entries?.some(
			(e) => e.pkg_id === pkgId
		)
	);
	const pin = usePinsStore((s) =>
		s.pins.find(
			(p) =>
				(p.kind === 'pkg-route' || p.kind === 'route') && pkgIdFromRoutePath(p.target) === pkgId
		)
	);
	const removePin = usePinsStore((s) => s.removePin);

	const openRoute = (path: string) => addTab(paneId, { kind: 'route', path });

	return (
		<>
			<DropdownMenuSeparator />
			<DropdownMenuItem onSelect={onReload} data-action="pkg.reload-view">
				<RefreshCw className="h-3.5 w-3.5" />
				Reload view
			</DropdownMenuItem>
			<DropdownMenuItem
				onSelect={() => openRoute(itemDetailPath(pkgId))}
				data-action="pkg.view-permissions"
			>
				<Shield className="h-3.5 w-3.5" />
				View permissions
			</DropdownMenuItem>
			<DropdownMenuItem
				onSelect={() => openRoute(itemDetailPath(pkgId))}
				data-action="pkg.package-settings"
			>
				<Settings className="h-3.5 w-3.5" />
				Package settings
			</DropdownMenuItem>
			{supervised && (
				<DropdownMenuItem
					onSelect={() => {
						pkgSupervisorRestart(pkgId).catch((e) =>
							console.warn(`[pkg-pane-menu] restart sidecar for ${pkgId} failed:`, e)
						);
					}}
					data-action="pkg.restart-sidecar"
				>
					<Bolt className="h-3.5 w-3.5" />
					Restart sidecar
				</DropdownMenuItem>
			)}
			<DropdownMenuSeparator />
			{pin && (
				<DropdownMenuItem
					onSelect={() => {
						removePin(pin.id).catch(() => {});
					}}
					data-action="pkg.unpin"
				>
					<PinOff className="h-3.5 w-3.5" />
					Unpin
				</DropdownMenuItem>
			)}
			<DropdownMenuItem
				onSelect={() => openRoute(VIOLATION_LOG_PATH)}
				data-action="pkg.report-violation-log"
			>
				<AlertTriangle className="h-3.5 w-3.5" />
				Report violation log
			</DropdownMenuItem>
		</>
	);
}
