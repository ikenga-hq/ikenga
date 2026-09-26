// WP-45 — pkg branch of the pane `⋯` menu (D-08 `pkg-view`,
// `designs/pane-chrome.html?state=pkg-view`, `pkgDotsMenu()`).
//
// Design order: Reload view · Open devtools · View permissions · Package
// settings · Restart sidecar ─ Unpin · Report violation log. The branch
// renders first in the menu (above Split right / Split down / Copy path),
// ending in its own separator. Every item is bound to an action that already
// exists, or omitted:
//
//   Reload view           → the pane's own refresh (`refreshPane` /
//                           the merged row's `onRefresh`), which re-runs the
//                           iframe host's fetch + handshake.
//   Open devtools         → OMITTED — accepted deviation from D-08 (the
//                           01 §Verification "⋯ menu is complete" line must
//                           carry it). The only devtools opener is the iyke
//                           HTTP bridge (`POST /iyke/devtools`, debug builds
//                           only, primary window); there is no Tauri command,
//                           and adding one is a needs-decision, not WP-45.
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
//                           Also where the sidecar-down strip's design "Log"
//                           link lands (the strip keeps one action: Restart).
//   Keep blocking         → shown only while a webview pane is parked on
//                           `pkg-blocked` (`pkg-blocked-store`): restores the
//                           native surface on its previous page.

import type { ReactNode } from 'react';
import { useQuery } from '@tanstack/react-query';
import { AlertTriangle, Ban, Bolt, PinOff, RefreshCw, Settings, Shield } from 'lucide-react';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneId, PaneView } from '@/lib/panes/types';
import {
	itemDetailPath,
	pkgIdFromRoutePath,
	VIOLATION_LOG_PATH,
} from '@/lib/pkg/pkg-view-state';
import { useKeepBlocking } from '@/lib/pkg/pkg-blocked-store';
import { usePinsStore } from '@/lib/shell/pins-store';
import { pkgKernelStatus, pkgSupervisorRestart } from '@/lib/tauri-cmd';
import type { MenuItemCondition } from '@/lib/actions/store';

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

/** What the pkg branch adds to the pane menu's `resolveMenuItems` call. */
export interface PkgPaneMenuData {
	conditions: Partial<Record<MenuItemCondition, boolean>>;
	handlers: Record<string, () => void>;
	icons: Record<string, ReactNode>;
}

/** A pane that shows no pkg: the pkg branch is skipped. */
export const NO_PKG_PANE_MENU: PkgPaneMenuData = { conditions: { 'pkg-pane': false }, handlers: {}, icons: {} };

/** The shipped glyphs of the pkg branch (`data-action` = the id, verbatim). */
const PKG_MENU_ICONS: Record<string, ReactNode> = {
	'pkg.keep-blocking': <Ban className="h-3.5 w-3.5" />,
	'pkg.reload-view': <RefreshCw className="h-3.5 w-3.5" />,
	'pkg.view-permissions': <Shield className="h-3.5 w-3.5" />,
	'pkg.package-settings': <Settings className="h-3.5 w-3.5" />,
	'pkg.restart-sidecar': <Bolt className="h-3.5 w-3.5" />,
	'pkg.unpin': <PinOff className="h-3.5 w-3.5" />,
	'pkg.report-violation-log': <AlertTriangle className="h-3.5 w-3.5" />,
};

/**
 * The pkg branch of the pane `⋯` menu (§1.3's `pane` menu, `pkg-*`
 * conditions) as conditions + handlers `pane-toolbar.tsx` folds into its own
 * `resolveMenuItems('pane', ...)` call — the whole menu (pkg branch, artifact
 * branch and the plain items) resolves and renders from one effective menu,
 * so reordering/hiding in `actions.json` reaches every branch, not just the
 * plain items.
 *
 * Only ever called for a pkg pane, and only while the `⋯` menu is open
 * (`PkgPaneMenuDataProvider`): the kernel-status query needs a
 * `QueryClient`, which a non-pkg pane never asks for.
 */
export function usePkgPaneMenuData(paneId: PaneId, pkgId: string, onReload: () => void): PkgPaneMenuData {
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
			(p) => (p.kind === 'pkg-route' || p.kind === 'route') && pkgIdFromRoutePath(p.target) === pkgId
		)
	);
	const removePin = usePinsStore((s) => s.removePin);
	const keepBlocking = useKeepBlocking(pkgId, paneId);

	const openRoute = (path: string) => addTab(paneId, { kind: 'route', path });

	return {
		conditions: {
			'pkg-pane': true,
			'pkg-blocking': Boolean(keepBlocking),
			'pkg-supervised': supervised,
			'pkg-pinned': Boolean(pin),
		},
		handlers: {
			'pkg.keep-blocking': () => keepBlocking?.(),
			'pkg.reload-view': onReload,
			'pkg.view-permissions': () => openRoute(itemDetailPath(pkgId)),
			'pkg.package-settings': () => openRoute(itemDetailPath(pkgId)),
			'pkg.restart-sidecar': () => {
				pkgSupervisorRestart(pkgId).catch((e) =>
					console.warn(`[pkg-pane-menu] restart sidecar for ${pkgId} failed:`, e)
				);
			},
			'pkg.unpin': () => {
				if (pin) removePin(pin.id).catch(() => {});
			},
			'pkg.report-violation-log': () => openRoute(VIOLATION_LOG_PATH),
		},
		icons: PKG_MENU_ICONS,
	};
}

/** Runs `usePkgPaneMenuData` and hands the result to `children` — mounted
 *  inside the open `⋯` menu, for pkg panes only. */
export function PkgPaneMenuDataProvider({
	paneId,
	pkgId,
	onReload,
	children,
}: {
	paneId: PaneId;
	pkgId: string;
	onReload: () => void;
	children: (data: PkgPaneMenuData) => ReactNode;
}) {
	return <>{children(usePkgPaneMenuData(paneId, pkgId, onReload))}</>;
}
