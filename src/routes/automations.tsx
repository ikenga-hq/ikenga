// /automations — Native automations surface (WP-42, D-07 `schedules` state).
//
// Replaces the pre-WP-42 landing (which only rendered "Nothing scheduled"
// and auto-navigated away into com.ikenga.agent-ops's iframe on `?view=`).
// The native view now lists schedules from all three sources
// (`src/shell/automations/use-automations.ts`); the agent-ops deep-link is
// preserved two ways: an explicit "Open in agent-ops" button when the pkg is
// installed, and — for anything that already bookmarked `?view=schedule` /
// `?view=runs` — the same auto-navigate-into-the-pkg behavior as before.

import { createFileRoute } from '@tanstack/react-router';
import { useEffect } from 'react';
import { z } from 'zod';

import { usePaneStore } from '@/lib/panes/pane-store';
import { usePkgMenuStore } from '@/lib/pkg/pkg-menu-store';
import { pkgKernelStatus } from '@/lib/tauri-cmd';
import { AutomationsView } from '@/shell/automations/automations-view';

const AGENT_OPS_PKG_ID = 'com.ikenga.agent-ops';
const SCHEDULE_PATH = `/pkg/${AGENT_OPS_PKG_ID}/schedule`;
const RUNS_PATH = `/pkg/${AGENT_OPS_PKG_ID}/runs`;

const automationsSearchSchema = z.object({
	view: z.enum(['schedule', 'runs']).optional(),
});

function AutomationsPage() {
	const search = Route.useSearch();
	const navigateFocused = usePaneStore((s) => s.navigateFocused);

	// Bookmarked `?view=` deep links still jump straight into the agent-ops
	// pkg iframe, same as the pre-WP-42 landing did. Fresh navigation to
	// `/automations` (no `view`) renders the native list instead.
	useEffect(() => {
		if (!search.view) return;
		let cancelled = false;
		(async () => {
			try {
				const status = await pkgKernelStatus();
				const reg = (status.registries.ui_routes ?? {}) as {
					entries?: Array<{ pkg_id: string; path: string }>;
				};
				const entries = reg.entries ?? [];
				const pkgRoutes = entries.filter((e) => e.pkg_id === AGENT_OPS_PKG_ID);
				if (cancelled || pkgRoutes.length === 0) return;
				const targetSubPath = search.view === 'runs' ? '/runs' : '/schedule';
				const hasTarget = pkgRoutes.some((e) => e.path === targetSubPath);
				if (!hasTarget) return;
				usePkgMenuStore
					.getState()
					.setActiveFeature(AGENT_OPS_PKG_ID, search.view === 'runs' ? 'v:runs' : 'v:schedule');
				navigateFocused(search.view === 'runs' ? RUNS_PATH : SCHEDULE_PATH);
			} catch {
				// pkg not installed or kernel unreachable — fall through to the native view
			}
		})();
		return () => {
			cancelled = true;
		};
	}, [navigateFocused, search.view]);

	return <AutomationsView />;
}

export const Route = createFileRoute('/automations')({
	component: AutomationsPage,
	validateSearch: automationsSearchSchema,
});
