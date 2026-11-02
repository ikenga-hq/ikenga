// Shared deep-link for /outbox/<view> shell routes → the com.ikenga.outbound
// pkg, mirroring /cron → agent-ops (cron.tsx). Forwards the focused pane to
// /pkg/com.ikenga.outbound/<view> (the registered sub-route if present, else the
// pkg root) and sets the pkg-menu active feature so an already-mounted iframe
// swaps views without a remount. Falls back to a landing when the pkg isn't
// installed. Since the outbox redesign (plans/outbox-redesign D-01) this covers
// /outbox/approvals too: the one triage inbox is the pkg's (route `/approvals`,
// view token `v:approvals`), and the shell keeps only the entry points.

import { Send } from 'lucide-react';
import type { ComponentType } from 'react';
import { useEffect } from 'react';
import { usePaneStore } from '@/lib/panes/pane-store';
import { usePkgMenuStore } from '@/lib/pkg/pkg-menu-store';
import { pkgKernelStatus } from '@/lib/tauri-cmd';

const OUTBOUND_PKG_ID = 'com.ikenga.outbound';

/** The shell's entry route for the Outbox queue (rail, status bar, ⌘K, menu,
 *  `pa-action-paused`): it deep-links into the outbound pkg's `/approvals`. */
export const OUTBOX_APPROVALS_ROUTE = '/outbox/approvals';

/** Is `path` a route of the outbound pkg (`/pkg/com.ikenga.outbound`, `…/<sub>`)? The
 *  rail's Outbox key is active there, and `pa-action-paused` does not yank a pane that is
 *  already in the pkg (the operator may be mid-edit; the pkg's own list picks the batch up). */
export function isOutboundPkgPath(path: string | null | undefined): boolean {
	const root = `/pkg/${OUTBOUND_PKG_ID}`;
	return !!path && (path === root || path.startsWith(`${root}/`) || path.startsWith(`${root}?`));
}

export function OutboundDeepLink({
	view,
	title,
	description,
	Icon = Send,
}: {
	view: string;
	title: string;
	description: string;
	Icon?: ComponentType<{ className?: string }>;
}) {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);

	useEffect(() => {
		let cancelled = false;
		(async () => {
			try {
				const status = await pkgKernelStatus();
				const reg = (status.registries.ui_routes ?? {}) as {
					entries?: Array<{ pkg_id: string; path: string }>;
				};
				const pkgRoutes = (reg.entries ?? []).filter((e) => e.pkg_id === OUTBOUND_PKG_ID);
				if (cancelled || pkgRoutes.length === 0) return;
				const sub = `/${view}`;
				const hasSub = pkgRoutes.some((e) => e.path === sub);
				usePkgMenuStore.getState().setActiveFeature(OUTBOUND_PKG_ID, `v:${view}`);
				navigateFocused(hasSub ? `/pkg/${OUTBOUND_PKG_ID}${sub}` : `/pkg/${OUTBOUND_PKG_ID}/`);
			} catch {
				// pkg not installed / kernel unreachable — show the landing below.
			}
		})();
		return () => {
			cancelled = true;
		};
	}, [navigateFocused, view]);

	return (
		<div className="flex h-full flex-col bg-background">
			<div className="border-b border-border px-4 py-3">
				<div className="flex items-center gap-2">
					<Icon className="h-5 w-5 text-muted-foreground" />
					<h1 className="text-lg font-semibold">{title}</h1>
				</div>
				<p className="mt-1 text-sm text-muted-foreground">{description}</p>
			</div>
			<div className="flex flex-1 flex-col items-center justify-center gap-4 p-8 text-center">
				<Icon className="h-12 w-12 text-muted-foreground/40" />
				<div className="max-w-sm">
					<div className="text-base font-medium">Outbound app not installed</div>
					<p className="mt-1 text-sm text-muted-foreground">
						Install the <code className="font-mono text-xs">com.ikenga.outbound</code> pkg to use
						this view. Once installed, this route redirects automatically.
					</p>
				</div>
				<button
					type="button"
					className="mt-2 rounded-md border border-border bg-muted px-4 py-2 text-sm hover:bg-accent hover:text-accent-foreground"
					onClick={() => navigateFocused('/packages')}
				>
					Browse packages
				</button>
			</div>
		</div>
	);
}
