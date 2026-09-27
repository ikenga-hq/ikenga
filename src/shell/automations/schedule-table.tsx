// The schedules list — D-07 `schedules` state's table: "name, cron in plain
// words + expression, target, engine, last run, next run" plus per-row
// actions (History, Run now, Pause/Resume, Edit, Delete). Every disabled
// action carries its reason as `title`, never a silently inert control
// (`06-interaction-spec.md` §1.2).

import { useMutation, useQueryClient } from '@tanstack/react-query';
import { Clock, History, Info, Pencil, Play } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { agentOpsRunNow, agentOpsSetEnabled } from '@/lib/tauri-cmd';
import { AGENT_OPS_JOBS_QUERY_KEY } from './use-automations';
import type { AutomationRow } from './types';

export interface ScheduleTableProps {
	rows: AutomationRow[];
	onOpenHistory: (row: AutomationRow) => void;
	onOpenEdit: (row: AutomationRow) => void;
}

const SOURCE_LABEL: Record<AutomationRow['source'], string> = {
	'agent-ops': 'agent-ops',
	'manifest-cron': 'from a package',
	workflow: 'from a package',
};

function RunNowButton({ row }: { row: AutomationRow }) {
	const qc = useQueryClient();
	const runNow = useMutation({
		mutationFn: async () => {
			if (!row.agentOpsJobId) return;
			const result = (await agentOpsRunNow(row.agentOpsJobId)) as {
				ok: boolean;
				message?: string;
				code?: string;
				error?: string;
			};
			if (!result.ok) throw new Error(`${result.code}: ${result.error}`);
			return result;
		},
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: AGENT_OPS_JOBS_QUERY_KEY });
		},
	});

	const disabledReason = row.runNowDisabledReason ?? (runNow.isPending ? 'Running…' : null);

	return (
		<Button
			type="button"
			size="sm"
			variant="ghost"
			className="h-8 gap-1 px-2 text-xs"
			disabled={disabledReason !== null}
			title={disabledReason ?? undefined}
			aria-busy={runNow.isPending}
			onClick={() => runNow.mutate()}
		>
			<Play className="h-3.5 w-3.5" />
			Run now
		</Button>
	);
}

function PauseButton({ row }: { row: AutomationRow }) {
	const qc = useQueryClient();
	const toggle = useMutation({
		mutationFn: async () => {
			if (!row.agentOpsJobId) return;
			const result = (await agentOpsSetEnabled(row.agentOpsJobId, row.paused)) as {
				ok: boolean;
				code?: string;
				error?: string;
			};
			if (!result.ok) throw new Error(`${result.code}: ${result.error}`);
		},
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: AGENT_OPS_JOBS_QUERY_KEY });
		},
	});

	return (
		<Button
			type="button"
			size="sm"
			variant="ghost"
			className="h-8 px-2 text-xs"
			disabled={row.pauseDisabledReason !== null || toggle.isPending}
			title={row.pauseDisabledReason ?? undefined}
			onClick={() => toggle.mutate()}
		>
			{row.paused ? 'Resume' : 'Pause'}
		</Button>
	);
}

export function ScheduleTable({ rows, onOpenHistory, onOpenEdit }: ScheduleTableProps) {
	return (
		<table className="w-full border-collapse text-sm" aria-label="Schedules">
			<thead>
				<tr className="border-b border-border text-left text-xs text-muted-foreground">
					<th className="py-2 pr-3 font-medium">Name</th>
					<th className="py-2 pr-3 font-medium">When</th>
					<th className="py-2 pr-3 font-medium">Runs</th>
					<th className="py-2 pr-3 font-medium">Engine</th>
					<th className="py-2 pr-3 font-medium">Last run</th>
					<th className="py-2 pr-3 font-medium">Next</th>
					<th className="py-2 pr-3 font-medium text-right">Actions</th>
				</tr>
			</thead>
			<tbody>
				{rows.map((row) => (
					<tr key={row.id} className="border-b border-border/60 align-top">
						<td className="py-2 pr-3">
							<div className="flex items-center gap-1.5">
								<span className="font-medium">{row.name}</span>
								{row.source !== 'agent-ops' && (
									<span className="rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
										{SOURCE_LABEL[row.source]}
									</span>
								)}
								{row.paused && (
									<span className="rounded bg-warning-soft px-1.5 py-0.5 text-[10px] text-warning">
										paused
									</span>
								)}
							</div>
							<div className="mt-0.5 font-mono text-[10px] text-muted-foreground" title={row.filePath}>
								{row.filePath}
							</div>
						</td>
						<td className="py-2 pr-3">
							{row.cronWords ? (
								<>
									<div>{row.cronWords}</div>
									<div className="font-mono text-[10px] text-muted-foreground">{row.cronExpr}</div>
								</>
							) : (
								<span className="text-muted-foreground">—</span>
							)}
						</td>
						<td className="py-2 pr-3 font-mono text-xs">{row.target}</td>
						<td className="py-2 pr-3 font-mono text-xs">{row.engine}</td>
						<td className="py-2 pr-3 text-xs">{row.lastRun}</td>
						<td className="py-2 pr-3 font-mono text-xs">{row.nextRun}</td>
						<td className="py-2 pr-3">
							<div className="flex items-center justify-end gap-1">
								<Button
									type="button"
									size="sm"
									variant="ghost"
									className="h-8 gap-1 px-2 text-xs"
									onClick={() => onOpenHistory(row)}
								>
									<History className="h-3.5 w-3.5" />
									History
								</Button>
								<RunNowButton row={row} />
								<PauseButton row={row} />
								<Button
									type="button"
									size="icon"
									variant="ghost"
									className="h-8 w-8"
									aria-label={
										row.editDisabledReason ? `Details for ${row.name} (read-only)` : `Edit ${row.name}`
									}
									title={row.editDisabledReason ?? undefined}
									onClick={() => onOpenEdit(row)}
								>
									{row.editDisabledReason ? <Info className="h-4 w-4" /> : <Pencil className="h-4 w-4" />}
								</Button>
							</div>
						</td>
					</tr>
				))}
			</tbody>
		</table>
	);
}

export function ScheduleTableCaption({ count }: { count: number }) {
	return (
		<h3 className="flex items-center gap-1.5 text-sm font-semibold">
			<Clock className="h-4 w-4 text-primary" />
			Schedules <span className="text-muted-foreground">{count}</span>
		</h3>
	);
}
