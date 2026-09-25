// The native `/automations` surface (WP-42, D-07 `schedules` state:
// designs/system-flows.html?state=schedules). Composes the three-source join
// (`useAutomations`), the table, the run-history drawer and the create/edit
// sheet, plus the D-07 `states` contact-sheet components (WP-43) for
// loading / error / empty.

import { useState } from 'react';
import { ExternalLink, Plus } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { EmptyState, ErrorState, LoadingState } from '@/components/states';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useAutomations } from './use-automations';
import { ScheduleTable, ScheduleTableCaption } from './schedule-table';
import { RunHistoryDrawer } from './run-history-drawer';
import { ScheduleEditSheet } from './schedule-edit-sheet';
import type { AutomationRow } from './types';

export function AutomationsView() {
	const { rows, isLoading, error, agentOpsPkgPath, refetch } = useAutomations();
	const [historyRow, setHistoryRow] = useState<AutomationRow | null>(null);
	const [editState, setEditState] = useState<{ open: boolean; row: AutomationRow | null }>({
		open: false,
		row: null,
	});
	const navigateFocused = usePaneStore((s) => s.navigateFocused);

	return (
		<div className="flex h-full flex-col bg-background text-foreground" data-state="schedules">
			<div className="flex items-center justify-between border-b border-border px-6 py-4">
				<h1 className="text-lg font-semibold">Automations</h1>
				<div className="flex items-center gap-2">
					{agentOpsPkgPath && (
						<Button
							type="button"
							size="sm"
							variant="outline"
							className="gap-1.5"
							onClick={() => navigateFocused(agentOpsPkgPath)}
						>
							<ExternalLink className="h-3.5 w-3.5" />
							Open in agent-ops
						</Button>
					)}
					<Button
						type="button"
						size="sm"
						className="gap-1.5"
						onClick={() => setEditState({ open: true, row: null })}
					>
						<Plus className="h-3.5 w-3.5" />
						New schedule
					</Button>
				</div>
			</div>

			<div className="flex-1 overflow-y-auto p-6">
				{isLoading ? (
					<LoadingState heading="Reading schedules…" fill data-state="loading" />
				) : error ? (
					<ErrorState
						heading="Couldn't read schedules"
						body={error.message}
						action={{ label: 'Retry', onClick: refetch }}
						fill
						data-state="error"
					/>
				) : rows.length === 0 ? (
					<EmptyState
						heading="Nothing scheduled"
						body="A schedule runs a skill, a workflow or a command on a clock, whether or not you are watching."
						action={{ label: 'New schedule', onClick: () => setEditState({ open: true, row: null }) }}
						fill
						data-state="empty"
					/>
				) : (
					<div className="rounded-lg border border-border p-4">
						<div className="mb-3 flex items-center justify-between">
							<ScheduleTableCaption count={rows.length} />
						</div>
						<ScheduleTable
							rows={rows}
							onOpenHistory={setHistoryRow}
							onOpenEdit={(row) => setEditState({ open: true, row })}
						/>
						<p className="mt-3 text-xs text-muted-foreground">
							A package-contributed row is read-only here: its manifest wins on the next reload.
						</p>
					</div>
				)}
			</div>

			<RunHistoryDrawer row={historyRow} onClose={() => setHistoryRow(null)} />
			<ScheduleEditSheet
				row={editState.row}
				open={editState.open}
				onOpenChange={(open) => setEditState((s) => ({ ...s, open }))}
			/>
		</div>
	);
}
