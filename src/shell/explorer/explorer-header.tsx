import { useState } from 'react';
import { ChevronsDownUp, FoldVertical } from 'lucide-react';
import { useShellStore } from '@/lib/shell/shell-store';

export function ExplorerHeader() {
	const activeProject = useShellStore((s) => s.activeProject);
	const explorerSections = useShellStore((s) => s.explorerSections);
	const setExplorerSectionCollapsed = useShellStore((s) => s.setExplorerSectionCollapsed);

	const [previousOpenState, setPreviousOpenState] = useState<string[] | null>(null);

	const allCollapsed = explorerSections.every((s) => s.collapsed);

	const handleCollapseToggle = () => {
		if (allCollapsed) {
			// Restore previous open set
			if (previousOpenState && previousOpenState.length > 0) {
				for (const sec of explorerSections) {
					setExplorerSectionCollapsed(sec.id, !previousOpenState.includes(sec.id));
				}
			} else {
				// Fallback: restore default open set (files, artifacts, sessions, automations)
				const defaults = new Set(['files', 'artifacts', 'sessions', 'automations']);
				for (const sec of explorerSections) {
					setExplorerSectionCollapsed(sec.id, !defaults.has(sec.id));
				}
			}
		} else {
			// Save current open sections
			const currentlyOpen = explorerSections.filter((s) => !s.collapsed).map((s) => s.id);
			setPreviousOpenState(currentlyOpen);
			// Collapse all
			for (const sec of explorerSections) {
				if (!sec.collapsed) {
					setExplorerSectionCollapsed(sec.id, true);
				}
			}
		}
	};

	const projectName = activeProject?.root_path?.split('/').pop() || 'No Project';

	return (
		<div className="flex shrink-0 items-center justify-between px-3 py-2 border-b border-border bg-background">
			<button
				type="button"
				className="text-[11px] font-semibold text-foreground hover:bg-accent hover:text-accent-foreground px-2 py-1 rounded cursor-pointer truncate max-w-[200px] min-h-[24px]"
				title="Switch project (⌘P)"
			>
				{projectName}
			</button>
			<div className="flex items-center gap-1">
				<button
					type="button"
					onClick={handleCollapseToggle}
					className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-accent-foreground min-h-[24px] min-w-[24px] flex items-center justify-center focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring focus:outline-none"
					title={allCollapsed ? 'Restore sections' : 'Collapse all sections'}
					aria-label={allCollapsed ? 'Restore sections' : 'Collapse all sections'}
				>
					{allCollapsed ? (
						<ChevronsDownUp className="h-3.5 w-3.5" />
					) : (
						<FoldVertical className="h-3.5 w-3.5" />
					)}
				</button>
			</div>
		</div>
	);
}
