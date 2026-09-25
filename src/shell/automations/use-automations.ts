// Data layer for the D-07 `/automations` view (WP-42). Joins three sources
// into one `AutomationRow[]`:
//   1. `agent-ops`    — `agentOpsListJobs()` (WP-09/G-TRIGGER host bridge).
//                       The only source with a live daemon: pause, run now,
//                       edit and delete are real here.
//   2. `manifest-cron`— `pkgKernelStatus().registries.cron` (the same typed
//                       `PkgCronEntry[]` read `ngwa-health-surface.tsx`'s Cron
//                       panel already uses for this exact data — DEC-33).
//                       Read-only: no native `cron_*` commands exist to write
//                       or run a bare manifest schedule (Round 32 G-45).
//   3. `workflow`     — declared `workflows[]` per installed pkg, via the
//                       WP-31 importers' `workflowGraphsFromManifest` (same
//                       join `src/shell/explorer/sections/automations.tsx`
//                       already does). Read-only for the same reason.
//
// A missing/unreadable agent-ops config (most machines never set one up) is
// not a page error — the source degrades to empty, same as one broken pkg
// manifest must not blank the workflow list (`explorer/sections/automations.tsx`).

import { useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import type { AgentOpsListJobsResult, AgentOpsRawJob } from '@ikenga/contract';
import { agentOpsListJobs, pkgKernelStatus, pkgPreviewManifest, type PkgKernelStatus } from '@/lib/tauri-cmd';
import { workflowGraphsFromManifest } from '@/shell/ngwa/use-pkg-workflow-graphs';
import { cronToWords, cronToWordsDialect } from './cron-words';
import { NO_RUNNER_REASON, READ_ONLY_MANIFEST_REASON, type AutomationRow } from './types';

export const AGENT_OPS_JOBS_QUERY_KEY = ['agent-ops', 'jobs'] as const;

export const AGENT_OPS_PKG_ID = 'com.ikenga.agent-ops';
export const AGENT_OPS_JOBS_FILE = '~/.atelier/skill-agent-ops/jobs.json';

interface PkgCronEntry {
	pkg_id: string;
	cron_id: string;
	expr: string;
	handler: string;
}

function readCronRegistry(status: PkgKernelStatus): PkgCronEntry[] {
	const reg = status.registries?.cron as { entries?: unknown } | undefined;
	if (!reg || !Array.isArray(reg.entries)) return [];
	return reg.entries as PkgCronEntry[];
}

function fmtMs(ms: number | null | undefined): string {
	if (ms === null || ms === undefined || !Number.isFinite(ms)) return '—';
	return new Date(ms).toISOString().slice(0, 19).replace('T', ' ');
}

function agentOpsRowsFrom(result: AgentOpsListJobsResult | null): AutomationRow[] {
	if (!result || !result.ok) return [];
	const jobs: AgentOpsRawJob[] = result.jobs ?? [];
	return jobs.map((j) => {
		const daemonDown = !result.daemon_up;
		const runNowDisabledReason = daemonDown ? 'The agent-ops daemon is not running.' : null;
		return {
			id: `agent-ops:${j.id}`,
			source: 'agent-ops',
			name: j.label || j.id,
			cronWords: cronToWordsDialect(j.schedule, j.schedule_dialect),
			cronExpr: j.schedule,
			target: `${j.mode} · ${j.command}`,
			engine: j.model ?? j.agent ?? (j.mode === 'agent' ? 'claude-code' : '—'),
			lastRun: j.state?.lastStatus ? `${j.state.lastStatus} · ${fmtMs(j.state?.lastRunAtMs)}` : 'never',
			nextRun: fmtMs(j.state?.nextRunAtMs),
			filePath: AGENT_OPS_JOBS_FILE,
			paused: !j.enabled,
			runNowDisabledReason,
			pauseDisabledReason: daemonDown ? 'The agent-ops daemon is not running.' : null,
			editDisabledReason: null,
			deleteDisabledReason: null,
			agentOpsJobId: j.id,
		} satisfies AutomationRow;
	});
}

async function workflowRows(status: PkgKernelStatus): Promise<AutomationRow[]> {
	const installed = status.installed.filter((p) => p.enabled && p.install_path);
	const perPkg = await Promise.all(
		installed.map(async (pkg) => {
			try {
				const manifest = await pkgPreviewManifest(pkg.install_path);
				const graphs = workflowGraphsFromManifest(
					pkg.id,
					`${pkg.install_path}/manifest.json`,
					(manifest as { workflows?: unknown }).workflows
				);
				return graphs.map((graph) => {
					const stepCount = graph.nodes.filter((n) => n.kind === 'step').length;
					return {
						id: `workflow:${graph.id}`,
						source: 'workflow',
						name: graph.title,
						cronWords: null,
						cronExpr: null,
						target: `workflow · ${graph.title} (${stepCount} step${stepCount === 1 ? '' : 's'})`,
						engine: '—',
						lastRun: '—',
						nextRun: '—',
						filePath: graph.source_path ?? `${pkg.install_path}/manifest.json`,
						paused: false,
						runNowDisabledReason: NO_RUNNER_REASON,
						pauseDisabledReason: READ_ONLY_MANIFEST_REASON,
						editDisabledReason: READ_ONLY_MANIFEST_REASON,
						deleteDisabledReason: READ_ONLY_MANIFEST_REASON,
						agentOpsJobId: null,
					} satisfies AutomationRow;
				});
			} catch {
				// One unreadable manifest must not blank the whole list.
				return [];
			}
		})
	);
	return perPkg.flat();
}

export interface UseAutomationsResult {
	rows: AutomationRow[];
	isLoading: boolean;
	error: Error | null;
	/** `com.ikenga.agent-ops` is installed and contributes a `ui.routes[]`
	 *  entry — used to preserve the pre-WP-42 deep-link as an explicit action. */
	agentOpsPkgPath: string | null;
	refetch: () => void;
}

export function useAutomations(): UseAutomationsResult {
	const kernelQuery = useQuery({
		queryKey: ['pkg-kernel-status'] as const,
		queryFn: pkgKernelStatus,
		retry: false,
	});

	const agentOpsQuery = useQuery({
		queryKey: AGENT_OPS_JOBS_QUERY_KEY,
		queryFn: async () => {
			try {
				return (await agentOpsListJobs()) as AgentOpsListJobsResult;
			} catch {
				// No jobs.json on disk (agent-ops never configured) — not an error,
				// just an empty source (mirrors the workflow join's degrade rule).
				return null;
			}
		},
		retry: false,
	});

	const workflowsQuery = useQuery({
		queryKey: ['automations', 'workflows', kernelQuery.dataUpdatedAt] as const,
		queryFn: () => workflowRows(kernelQuery.data as PkgKernelStatus),
		enabled: !!kernelQuery.data,
		retry: false,
	});

	const rows = useMemo(() => {
		if (!kernelQuery.data) return [];
		const status = kernelQuery.data;
		const agentOpsRows = agentOpsRowsFrom(agentOpsQuery.data ?? null);
		const cronRows = manifestCronRows(status);
		return [...agentOpsRows, ...cronRows, ...(workflowsQuery.data ?? [])];
	}, [kernelQuery.data, agentOpsQuery.data, workflowsQuery.data]);

	const agentOpsPkgPath = useMemo(() => {
		if (!kernelQuery.data) return null;
		const installed = kernelQuery.data.installed.some((p) => p.id === AGENT_OPS_PKG_ID && p.enabled);
		if (!installed) return null;
		const reg = kernelQuery.data.registries?.ui_routes as
			| { entries?: Array<{ pkg_id: string; path: string }> }
			| undefined;
		const hasRoute = (reg?.entries ?? []).some((e) => e.pkg_id === AGENT_OPS_PKG_ID);
		return hasRoute ? `/pkg/${AGENT_OPS_PKG_ID}/` : null;
	}, [kernelQuery.data]);

	return {
		rows,
		isLoading: kernelQuery.isLoading || agentOpsQuery.isLoading,
		error: (kernelQuery.error as Error | null) ?? null,
		agentOpsPkgPath,
		refetch: () => {
			void kernelQuery.refetch();
			void agentOpsQuery.refetch();
			void workflowsQuery.refetch();
		},
	};
}

// The cron registry read is synchronous (it's already in `pkgKernelStatus()`,
// no per-pkg fetch needed) — only the workflow join needs a second manifest
// fetch per pkg, hence that one being a separate async query.
function manifestCronRows(status: PkgKernelStatus): AutomationRow[] {
	const entries = readCronRegistry(status);
	const pkgById = new Map(status.installed.map((p) => [p.id, p]));
	return entries.map((c) => {
		const pkg = pkgById.get(c.pkg_id);
		const filePath = pkg ? `${pkg.install_path}/manifest.json` : `${c.pkg_id}/manifest.json`;
		return {
			id: `manifest-cron:${c.pkg_id}:${c.cron_id}`,
			source: 'manifest-cron',
			name: c.cron_id,
			cronWords: cronToWords(c.expr),
			cronExpr: c.expr,
			target: `${c.pkg_id} · ${c.handler}`,
			engine: '—',
			lastRun: '—',
			nextRun: '—',
			filePath,
			paused: false,
			runNowDisabledReason: NO_RUNNER_REASON,
			pauseDisabledReason: READ_ONLY_MANIFEST_REASON,
			editDisabledReason: READ_ONLY_MANIFEST_REASON,
			deleteDisabledReason: READ_ONLY_MANIFEST_REASON,
			agentOpsJobId: null,
		} satisfies AutomationRow;
	});
}
