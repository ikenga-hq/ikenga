import { useCallback } from 'react';
import { Clock } from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { ExplorerSectionContext } from '../section-registry';

export const automationsContextMenu = [
	{ id: 'run-now', label: 'Run now', run: () => {} },
	{ id: 'pause-resume', label: 'Pause / Resume', run: () => {} },
	{ id: 'open-definition', label: 'Open definition file', run: () => {} },
	{ id: 'open-last-log', label: 'Open last run log', run: () => {} },
	{ id: 'open-in-ngwa', label: 'Open in Ngwa', run: () => {} },
];

export interface AutomationItem {
	id: string;
	name: string;
	schedule: string;
	status?: 'ok' | 'running' | 'failed' | 'paused';
}

export function AutomationsSection({ projectId }: ExplorerSectionContext) {
	const query = useQuery<AutomationItem[]>({
		queryKey: ['explorer-automations', projectId],
		queryFn: async () => {
			// In Phase 1 automations read from agent-ops or project definitions
			return [];
		},
		staleTime: 30_000,
	});

	const items = query.data ?? [];

	const openAutomations = useCallback(() => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: '/automations' });
	}, []);

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
			{items.map((item) => (
				<ListRow
					key={item.id}
					size="sm"
					onActivate={openAutomations}
					title={item.name}
					className="w-full gap-1.5 px-2"
				>
					<Clock className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
					<span className="flex-1 truncate text-xs">{item.name}</span>
					<span className="text-[10px] text-muted-foreground font-mono">{item.schedule}</span>
				</ListRow>
			))}
		</div>
	);
}
