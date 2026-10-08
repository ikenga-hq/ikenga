import { useCallback } from 'react';
import { Clock, GitBranch } from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import { pkgKernelStatus, pkgPreviewManifest } from '@/lib/tauri-cmd';
import { workflowGraphsFromManifest } from '@/shell/ngwa/use-pkg-workflow-graphs';
import { cronToWords } from '@/shell/automations/cron-words';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import type { ExplorerSectionContext } from '../section-registry';
import { errorMessageOf, SectionErrorRow } from '../section-error-row';

export interface AutomationItem {
	id: string;
	name: string;
	/** Cron expression for a schedule; step count for a declared workflow. */
	schedule: string;
	status?: 'ok' | 'running' | 'failed' | 'paused';
	kind?: 'workflow' | 'schedule';
}

/**
 * List the §10 `workflows[]` entries declared across installed pkgs
 * (WP-31 review fix, Round 29).
 *
 * Neither `ngwa_snapshot` nor `pkg_kernel_status` exposes `workflows[]` — the
 * snapshot is the G-NGWA-ITEM join and kernel status is
 * `installed[] + registries + api_version` — so this walks the kernel's
 * `installed[]` and reads each pkg's manifest through the existing
 * `pkg_preview_manifest` command. No new Tauri command, no ACL change.
 *
 * Pkgs whose manifest couldn't be read are pushed onto `unreadable` (by id)
 * so the section can say so instead of implying they declare nothing.
 *
 * Exported for the section's test.
 */
export async function listDeclaredWorkflows(unreadable: string[] = []): Promise<AutomationItem[]> {
	const status = await pkgKernelStatus();
	const installed = (status.installed ?? []).filter((p) => p.enabled && p.install_path);

	const perPkg = await Promise.all(
		installed.map(async (pkg) => {
			try {
				const manifest = await pkgPreviewManifest(pkg.install_path);
				return workflowGraphsFromManifest(
					pkg.id,
					`${pkg.install_path}/manifest.json`,
					(manifest as { workflows?: unknown }).workflows,
				);
			} catch (err) {
				// One unreadable manifest must not blank the whole section —
				// but it is reported, not read as "declares no workflows".
				console.warn(`[explorer/automations] manifest read failed for ${pkg.id}`, err);
				unreadable.push(pkg.id);
				return [];
			}
		}),
	);

	return perPkg.flat().map((graph) => ({
		id: graph.id,
		name: graph.title,
		schedule: `${graph.nodes.length} step${graph.nodes.length === 1 ? '' : 's'}`,
		kind: 'workflow' as const,
	}));
}

interface PkgCronEntry {
	pkg_id: string;
	cron_id: string;
	expr: string;
	handler: string;
}

/**
 * List manifest `cron[]` entries across installed pkgs (WP-42 — Round 32 G-45
 * follow-up: "manifest cron[] still unlisted"). Reads the same typed
 * `registries.cron` the WP-16 Ngwa Health Surface's Cron panel already uses
 * for this data (`ngwa-health-surface.tsx`, DEC-33) rather than re-parsing
 * `ngwa_snapshot`'s composite description string.
 *
 * Exported for the section's test.
 */
export async function listCronSchedules(): Promise<AutomationItem[]> {
	const status = await pkgKernelStatus();
	const reg = (status.registries?.cron ?? {}) as { entries?: PkgCronEntry[] };
	const entries = reg.entries ?? [];
	return entries.map((c) => ({
		id: `schedule:${c.pkg_id}:${c.cron_id}`,
		name: c.cron_id,
		schedule: cronToWords(c.expr),
		kind: 'schedule' as const,
	}));
}

export function AutomationsSection({ projectId }: ExplorerSectionContext) {
	const query = useQuery<{ items: AutomationItem[]; unreadable: string[] }>({
		queryKey: ['explorer-automations', projectId],
		queryFn: async () => {
			const unreadable: string[] = [];
			const [workflows, schedules] = await Promise.all([
				listDeclaredWorkflows(unreadable),
				listCronSchedules(),
			]);
			return { items: [...schedules, ...workflows], unreadable };
		},
		staleTime: 30_000,
		retry: false,
	});

	const items = query.data?.items ?? [];
	const unreadable = query.data?.unreadable ?? [];

	const openAutomations = useCallback(() => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: '/automations' });
	}, []);

	const openNgwa = useCallback(() => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: '/ngwa/installed' });
	}, []);

	const openRuns = useCallback(() => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: '/automations?view=runs' });
	}, []);

	if (query.isError) {
		return (
			<SectionErrorRow
				message="Couldn't load automations"
				detail={errorMessageOf(query.error)}
				onRetry={() => void query.refetch()}
			/>
		);
	}

	const unreadableRow =
		unreadable.length > 0 ? (
			<SectionErrorRow
				message={`Couldn't read workflows for ${unreadable.length === 1 ? '1 package' : `${unreadable.length} packages`}`}
				detail={unreadable.join(', ')}
				onRetry={() => void query.refetch()}
			/>
		) : null;

	if (items.length === 0 && unreadableRow) {
		return <div className="py-1">{unreadableRow}</div>;
	}

	if (items.length === 0) {
		return (
			<div className="p-4 text-center">
				<h3 className="text-sm font-semibold">Nothing scheduled</h3>
				<p className="text-xs text-muted-foreground mt-1 mb-3">
					A schedule runs a skill, a workflow or a command on a clock, whether or not you are watching.
				</p>
				<button
					type="button"
					onClick={openAutomations}
					className="text-xs bg-primary text-primary-foreground px-3 py-1.5 rounded hover:bg-primary/90 transition-colors"
				>
					New schedule
				</button>
			</div>
		);
	}

	return (
		<div className="py-1">
			{unreadableRow}
			{items.map((item) => (
				<EffectiveContextMenu
					key={item.id}
					menuId="automations"
					// A-9: `run-now`, `pause-resume` and `open-definition` are left
					// out — Ngwa's `workflows[]` / `cron[]` registries are read-only
					// lists with no per-item trigger, pause or definition-file
					// endpoint, and a row that only navigates is not that behaviour.
					builtinsNeedHandler
					handlers={{
						'open-last-log': openRuns,
						'open-in-ngwa': openNgwa,
					}}
				>
					<ListRow
						size="sm"
						onActivate={openAutomations}
						title={item.name}
						className="w-full gap-1.5 px-2"
					>
						{item.kind === 'workflow' ? (
							<GitBranch className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
						) : (
							<Clock className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
						)}
						<span className="flex-1 truncate text-xs">{item.name}</span>
						<span className="text-[10px] text-muted-foreground font-mono">{item.schedule}</span>
					</ListRow>
				</EffectiveContextMenu>
			))}
		</div>
	);
}
