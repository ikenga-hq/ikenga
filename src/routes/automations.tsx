// /automations — Schedules and agent runs (WP-10).
//
// Central surface for automations (schedules, cron jobs, background runs).
// Absorbs legacy /cron and /agent-runs. Deep-links into com.ikenga.agent-ops
// if installed, or renders native automations landing.

import { createFileRoute } from '@tanstack/react-router';
import { Clock, Plus } from 'lucide-react';
import { useEffect } from 'react';
import { z } from 'zod';

import { usePaneStore } from '@/lib/panes/pane-store';
import { usePkgMenuStore } from '@/lib/pkg/pkg-menu-store';
import { pkgKernelStatus } from '@/lib/tauri-cmd';
import { Button } from '@/components/ui/button';

const AGENT_OPS_PKG_ID = 'com.ikenga.agent-ops';
const SCHEDULE_PATH = `/pkg/${AGENT_OPS_PKG_ID}/schedule`;
const RUNS_PATH = `/pkg/${AGENT_OPS_PKG_ID}/runs`;
const PKG_ROOT = `/pkg/${AGENT_OPS_PKG_ID}/`;

const automationsSearchSchema = z.object({
	view: z.enum(['schedule', 'runs']).optional(),
});

function AutomationsPage() {
	const search = Route.useSearch();
	const navigateFocused = usePaneStore((s) => s.navigateFocused);

	useEffect(() => {
		let cancelled = false;
		(async () => {
			try {
				const status = await pkgKernelStatus();
				const reg = (status.registries.ui_routes ?? {}) as {
					entries?: Array<{ pkg_id: string; path: string }>;
				};
				const entries = reg.entries ?? [];
				const pkgRoutes = entries.filter((e) => e.pkg_id === AGENT_OPS_PKG_ID);
				if (cancelled || pkgRoutes.length === 0) {
					return;
				}
				const targetSubPath = search.view === 'runs' ? '/runs' : '/schedule';
				const hasTarget = pkgRoutes.some((e) => e.path === targetSubPath);
				usePkgMenuStore.getState().setActiveFeature(
					AGENT_OPS_PKG_ID,
					search.view === 'runs' ? 'v:runs' : 'v:schedule'
				);
				navigateFocused(hasTarget ? (search.view === 'runs' ? RUNS_PATH : SCHEDULE_PATH) : PKG_ROOT);
			} catch {
				// pkg not installed or kernel unreachable — fallback to landing
			}
		})();
		return () => {
			cancelled = true;
		};
	}, [navigateFocused, search.view]);

	return (
		<div className="flex h-full flex-col bg-background text-foreground">
			<div className="border-b border-border px-6 py-4 flex items-center justify-between">
				<div className="flex items-center gap-2.5">
					<Clock className="h-5 w-5 text-primary" />
					<h1 className="text-lg font-semibold">Automations</h1>
				</div>
			</div>

			<div className="flex-1 overflow-y-auto p-6">
				<div className="mx-auto max-w-2xl text-center py-16">
					<Clock className="mx-auto h-12 w-12 text-muted-foreground/40 mb-4" />
					<h2 className="text-lg font-medium text-foreground mb-2">Nothing scheduled</h2>
					<p className="text-sm text-muted-foreground mb-6 max-w-md mx-auto">
						A schedule runs a skill, a workflow or a command on a clock, whether or not you are watching.
					</p>
					<Button
						type="button"
						size="sm"
						onClick={() => {
							// Open command palette or trigger new automation schedule
						}}
					>
						<Plus className="mr-1.5 h-4 w-4" />
						New schedule
					</Button>
				</div>
			</div>
		</div>
	);
}

export const Route = createFileRoute('/automations')({
	component: AutomationsPage,
	validateSearch: automationsSearchSchema,
});
