// Create / edit modal (D-07 `schedules` state, locked design's `openSchEdit()`
// — a centered modal, not an edge panel: designs/system-flows.html:3419-3454).
// The only writable schedule store that exists is the agent-ops daemon's
// project-scoped config — there is no native `cron_*` command to write a bare
// manifest schedule or a declared `workflows[]` entry back to disk (Round 32
// G-45), so "New schedule" always creates an `agent-ops` job, and editing a
// `manifest-cron` or `workflow` row opens this dialog in its read-only,
// disabled-with-reason form instead of a blank form pretending it could save.
//
// The locked design's "Runs" selector has four segments (Skill / Workflow /
// Shell command / Dispatch to Chi), but the daemon's job schema only has two
// real execution modes (`mode: 'agent' | 'script'`, agent_ops.rs build_job).
// Skill and Workflow both map onto `agent` mode (the command string names a
// skill or a workflow; the daemon runs `claude -p` either way) — Dispatch to
// Chi has no backing command at all, so it stays selectable-but-disabled with
// an honest reason rather than silently no-opping on save (§1.2).

import { useState } from 'react';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { agentOpsDeleteJob, agentOpsUpsertJob } from '@/lib/tauri-cmd';
import { cronToWords } from './cron-words';
import { AGENT_OPS_JOBS_FILE, AGENT_OPS_JOBS_QUERY_KEY } from './use-automations';
import type { AutomationRow } from './types';

export interface ScheduleEditSheetProps {
	/** `null` row with `open: true` = "New schedule". */
	row: AutomationRow | null;
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

type RunType = 'skill' | 'workflow' | 'shell' | 'chi';

const RUN_TYPES: Array<{ value: RunType; label: string; disabledReason: string | null }> = [
	{ value: 'skill', label: 'Skill', disabledReason: null },
	{ value: 'workflow', label: 'Workflow', disabledReason: null },
	{ value: 'shell', label: 'Shell command', disabledReason: null },
	{
		value: 'chi',
		label: 'Dispatch to Chi',
		disabledReason: 'No native command dispatches a schedule to Chi yet — the daemon only runs an agent or a shell command.',
	},
];

function modeFromRunType(runType: RunType): 'agent' | 'script' {
	return runType === 'shell' ? 'script' : 'agent';
}

interface FormState {
	id: string;
	label: string;
	schedule: string;
	command: string;
	runType: RunType;
	model: string;
	timezone: string;
	enabled: boolean;
}

function initialForm(row: AutomationRow | null): FormState {
	if (row && row.source === 'agent-ops' && row.agentOpsJobId) {
		return {
			id: row.agentOpsJobId,
			label: row.name,
			schedule: row.cronExpr ?? '0 9 * * *',
			command: row.target.replace(/^(agent|script) · /, ''),
			// The daemon only records `agent` | `script` (build_job) — an
			// existing `agent`-mode job could have been created as either
			// Skill or Workflow, so this can only default to the more common
			// case, not round-trip which one it was.
			runType: row.target.startsWith('script') ? 'shell' : 'skill',
			model: '',
			// Round-trip the job's real timezone so Save never silently
			// rewrites it to UTC (WP-42-F0) — AgentOpsRawJob.timezone via
			// agentOpsRowsFrom, never a hardcoded default for an existing row.
			timezone: row.timezone,
			enabled: !row.paused,
		};
	}
	return {
		id: '',
		label: '',
		schedule: '0 9 * * *',
		command: '',
		runType: 'skill',
		model: '',
		timezone: 'UTC',
		enabled: true,
	};
}

export function ScheduleEditSheet({ row, open, onOpenChange }: ScheduleEditSheetProps) {
	const qc = useQueryClient();
	const isNew = row === null;
	const readOnly = row !== null && row.source !== 'agent-ops';
	const [form, setForm] = useState<FormState>(() => initialForm(row));
	const [error, setError] = useState<string | null>(null);

	const save = useMutation({
		mutationFn: async () => {
			const jobId = form.id || form.label.trim().toLowerCase().replace(/[^a-z0-9-]+/g, '-');
			const result = (await agentOpsUpsertJob({
				id: jobId,
				label: form.label.trim(),
				schedule: form.schedule.trim(),
				command: form.command.trim(),
				mode: modeFromRunType(form.runType),
				timezone: form.timezone,
				enabled: form.enabled,
				...(form.model.trim() ? { model: form.model.trim() } : {}),
			})) as { ok: boolean; code?: string; error?: string };
			if (!result.ok) throw new Error(`${result.code}: ${result.error}`);
			return result;
		},
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: AGENT_OPS_JOBS_QUERY_KEY });
			onOpenChange(false);
		},
		onError: (e) => setError(e instanceof Error ? e.message : String(e)),
	});

	const remove = useMutation({
		mutationFn: async () => {
			if (!form.id) return;
			const result = (await agentOpsDeleteJob(form.id)) as { ok: boolean; code?: string; error?: string };
			if (!result.ok) throw new Error(`${result.code}: ${result.error}`);
		},
		onSuccess: () => {
			void qc.invalidateQueries({ queryKey: AGENT_OPS_JOBS_QUERY_KEY });
			onOpenChange(false);
		},
		onError: (e) => setError(e instanceof Error ? e.message : String(e)),
	});

	return (
		<Dialog
			open={open}
			onOpenChange={(next) => {
				if (!next) setForm(initialForm(row));
				setError(null);
				onOpenChange(next);
			}}
		>
			{/* Centered modal per the locked design's `openModal('mid', ...)`
			    (designs/system-flows.html:3422), not an edge panel. */}
			<DialogContent className="flex max-h-[85vh] w-full max-w-[480px] flex-col" data-state="schedules">
				<DialogHeader>
					<DialogTitle>{isNew ? 'New schedule' : readOnly ? row.name : 'Edit schedule'}</DialogTitle>
					<DialogDescription>
						{readOnly ? row?.filePath : isNew ? `Writes ${AGENT_OPS_JOBS_FILE}` : row?.filePath}
					</DialogDescription>
				</DialogHeader>

				{readOnly ? (
					<div className="flex-1 space-y-3 overflow-y-auto">
						<p className="text-sm text-muted-foreground">{row?.editDisabledReason}</p>
						<dl className="space-y-2 text-xs">
							<div>
								<dt className="text-muted-foreground">Target</dt>
								<dd className="font-mono">{row?.target}</dd>
							</div>
							{row?.cronExpr && (
								<div>
									<dt className="text-muted-foreground">Schedule</dt>
									<dd>
										{row.cronWords} <span className="font-mono text-muted-foreground">({row.cronExpr})</span>
									</dd>
								</div>
							)}
						</dl>
					</div>
				) : (
					<div className="flex-1 space-y-3 overflow-y-auto">
						<label className="block text-xs font-medium text-muted-foreground" htmlFor="sched-name">
							Name
						</label>
						<input
							id="sched-name"
							className="h-9 w-full rounded border border-input bg-background px-2 text-sm"
							value={form.label}
							placeholder="weekly-digest"
							onChange={(e) => setForm((f) => ({ ...f, label: e.target.value }))}
						/>

						<label className="block text-xs font-medium text-muted-foreground" htmlFor="sched-cron">
							When (cron expression)
						</label>
						<input
							id="sched-cron"
							className="h-9 w-full rounded border border-input bg-background px-2 font-mono text-sm"
							value={form.schedule}
							onChange={(e) => setForm((f) => ({ ...f, schedule: e.target.value }))}
						/>
						<p className="text-xs text-muted-foreground">
							{cronToWords(form.schedule)} — derived from the expression, never typed separately.
						</p>

						<label className="block text-xs font-medium text-muted-foreground" htmlFor="sched-timezone">
							Timezone
						</label>
						<input
							id="sched-timezone"
							className="h-9 w-full rounded border border-input bg-background px-2 font-mono text-sm"
							value={form.timezone}
							placeholder="Africa/Lagos"
							onChange={(e) => setForm((f) => ({ ...f, timezone: e.target.value }))}
						/>

						<span className="block text-xs font-medium text-muted-foreground">Runs</span>
						<div className="inline-flex overflow-hidden rounded border border-input" role="radiogroup" aria-label="Runs">
							{RUN_TYPES.map((rt, i) => (
								<button
									key={rt.value}
									type="button"
									role="radio"
									aria-checked={form.runType === rt.value}
									disabled={rt.disabledReason !== null}
									title={rt.disabledReason ?? undefined}
									className={`h-7 px-3 text-xs ${i > 0 ? 'border-l border-input' : ''} ${
										form.runType === rt.value ? 'bg-primary/10 font-medium text-foreground' : 'text-muted-foreground'
									} ${rt.disabledReason !== null ? 'cursor-not-allowed opacity-50' : ''}`}
									onClick={() => setForm((f) => ({ ...f, runType: rt.value }))}
								>
									{rt.label}
								</button>
							))}
						</div>

						<label className="block text-xs font-medium text-muted-foreground" htmlFor="sched-command">
							{form.runType === 'shell' ? 'Shell command' : 'What to run'}
						</label>
						<input
							id="sched-command"
							className="h-9 w-full rounded border border-input bg-background px-2 font-mono text-sm"
							value={form.command}
							placeholder="release-status"
							onChange={(e) => setForm((f) => ({ ...f, command: e.target.value }))}
						/>

						<label className="block text-xs font-medium text-muted-foreground" htmlFor="sched-model">
							Model / engine (optional)
						</label>
						<input
							id="sched-model"
							className="h-9 w-full rounded border border-input bg-background px-2 text-sm"
							value={form.model}
							placeholder="claude-code"
							onChange={(e) => setForm((f) => ({ ...f, model: e.target.value }))}
						/>

						<p className="text-xs text-muted-foreground">Missed runs are not replayed.</p>

						{error && <p className="text-sm text-destructive">{error}</p>}
					</div>
				)}

				<DialogFooter className="mt-4">
					{!readOnly && !isNew && (
						<Button
							type="button"
							variant="destructive"
							onClick={() => remove.mutate()}
							disabled={remove.isPending}
							className="mr-auto"
						>
							Delete
						</Button>
					)}
					{!readOnly && (
						<Button
							type="button"
							onClick={() => save.mutate()}
							disabled={save.isPending || !form.label.trim() || !form.schedule.trim() || !form.command.trim()}
						>
							{isNew ? 'Create' : 'Save'}
						</Button>
					)}
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}
