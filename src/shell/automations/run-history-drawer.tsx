// Run-history drawer (D-07 `schedules` state — "run history drawer with
// log"). Locked design's `.drawer` (designs/system-flows.html:1323-1328) is a
// bottom-anchored, 190px-tall, full-width strip, not a right-side panel — the
// `side="bottom"` Sheet variant below matches that footprint. Only
// `agent-ops` rows have a real log: `agentOpsTailRun` reads the daemon's
// per-run tail file. The other two sources have no execution engine wired
// (Round 32 G-45), so the drawer says so rather than pretending there is
// history to show.

import { useQuery } from '@tanstack/react-query';
import {
	Sheet,
	SheetContent,
	SheetDescription,
	SheetHeader,
	SheetTitle,
} from '@/components/ui/sheet';
import { LoadingState } from '@/components/states';
import { agentOpsTailRun } from '@/lib/tauri-cmd';
import type { AutomationRow } from './types';

export interface RunHistoryDrawerProps {
	row: AutomationRow | null;
	onClose: () => void;
}

export function RunHistoryDrawer({ row, onClose }: RunHistoryDrawerProps) {
	const jobId = row?.agentOpsJobId ?? null;
	const tailQuery = useQuery({
		queryKey: ['automations', 'run-history', jobId] as const,
		queryFn: () => agentOpsTailRun(jobId!, 0),
		enabled: jobId !== null,
		retry: false,
	});

	return (
		<Sheet open={row !== null} onOpenChange={(open) => !open && onClose()}>
			<SheetContent side="bottom" className="flex h-[190px] w-full flex-col gap-1 py-3">
				{row && (
					// Radix owns `data-state` on SheetContent itself (open/closed, drives
					// the slide animation) — the G-55 state marker goes on an inner
					// wrapper instead of clobbering it. There is no separate addressable
					// design state for the drawer (only `schedules` itself is defState'd
					// in system-flows.html) — this reflects the `schedules` state with
					// the drawer opened via a row's History action, not a `?state=`
					// value of its own (WP-42-F4).
					<div className="flex flex-1 flex-col overflow-hidden" data-state="schedules">
						<SheetHeader className="shrink-0">
							<SheetTitle>Run history</SheetTitle>
							<SheetDescription>{row.name}</SheetDescription>
						</SheetHeader>
						<div className="flex-1 overflow-y-auto">
							{row.source !== 'agent-ops' ? (
								<p className="text-sm text-muted-foreground">
									No run history — no execution engine is wired for this source (the shell only
									observes the agent-ops daemon). File: <span className="font-mono">{row.filePath}</span>
								</p>
							) : tailQuery.isLoading ? (
								<LoadingState heading="Reading run history…" data-state="loading" />
							) : tailQuery.error ? (
								<p className="text-sm text-destructive">
									Couldn't read run history: {String(tailQuery.error)}
								</p>
							) : !tailQuery.data?.ok ? (
								<p className="text-sm text-destructive">
									{tailQuery.data ? `${tailQuery.data.code}: ${tailQuery.data.error}` : 'No result.'}
								</p>
							) : tailQuery.data.mode === 'agent' ? (
								<p className="text-sm text-muted-foreground">
									Agent-mode jobs keep no tail output. Status:{' '}
									{tailQuery.data.status ?? '—'}
								</p>
							) : (
								<pre className="whitespace-pre-wrap break-words rounded border border-border bg-muted/30 p-3 font-mono text-xs">
									{tailQuery.data.chunk || 'No output recorded for the last run.'}
								</pre>
							)}
						</div>
					</div>
				)}
			</SheetContent>
		</Sheet>
	);
}
