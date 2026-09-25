// Create / edit sheet (D-07 `schedules` state). The only writable schedule
// store that exists is the agent-ops daemon's project-scoped config — there
// is no native `cron_*` command to write a bare manifest schedule or a
// declared `workflows[]` entry back to disk (Round 32 G-45), so "New
// schedule" always creates an `agent-ops` job, and editing a `manifest-cron`
// or `workflow` row opens this sheet in its read-only, disabled-with-reason
// form instead of a blank form pretending it could save.

import { useState } from 'react';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import {
	Sheet,
	SheetContent,
	SheetDescription,
	SheetFooter,
	SheetHeader,
	SheetTitle,
} from '@/components/ui/sheet';
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

interface FormState {
	id: string;
	label: string;
	schedule: string;
	command: string;
	mode: 'agent' | 'script';
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
			mode: row.target.startsWith('script') ? 'script' : 'agent',
			model: '',
			timezone: 'UTC',
			enabled: !row.paused,
		};
	}
	return {
		id: '',
		label: '',
		schedule: '0 9 * * *',
		command: '',
		mode: 'agent',
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
				mode: form.mode,
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
		<Sheet
			open={open}
			onOpenChange={(next) => {
				if (!next) setForm(initialForm(row));
				setError(null);
				onOpenChange(next);
			}}
		>
			<SheetContent side="right" className="flex w-[420px] flex-col sm:max-w-[420px]">
				{/* Radix owns `data-state` on SheetContent itself (open/closed, drives
				    the slide animation) — the G-55 state marker goes on this inner
				    wrapper instead of clobbering it. */}
				<div className="flex flex-1 flex-col" data-state="schedules-edit">
					<SheetHeader>
						<SheetTitle>{isNew ? 'New schedule' : readOnly ? row.name : 'Edit schedule'}</SheetTitle>
						<SheetDescription>
							{readOnly ? row?.filePath : isNew ? `Writes ${AGENT_OPS_JOBS_FILE}` : row?.filePath}
						</SheetDescription>
					</SheetHeader>

					{readOnly ? (
						<div className="mt-4 flex-1 space-y-3">
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
						<div className="mt-4 flex-1 space-y-3 overflow-y-auto">
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

							<label className="block text-xs font-medium text-muted-foreground" htmlFor="sched-command">
								Command
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

					<SheetFooter className="mt-4">
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
					</SheetFooter>
				</div>
			</SheetContent>
		</Sheet>
	);
}
