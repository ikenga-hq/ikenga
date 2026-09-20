import { useState } from 'react';
import { ChevronsDownUp, FoldVertical, ChevronDown, Folder, Plus } from 'lucide-react';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { useShellStore } from '@/lib/shell/shell-store';
import { usePaneStore } from '@/lib/panes/pane-store';
import { labelFor } from '@/lib/keymap/registry';
import { cn } from '@/components/ui/utils';

export function ExplorerHeader() {
	const activeProject = useShellStore((s) => s.activeProject);
	const rawProjects = useShellStore((s) => s.projects);
	const projects = rawProjects ?? [];
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const setActiveProject = useShellStore((s) => s.setActiveProject);
	const explorerSections = useShellStore((s) => s.explorerSections);
	const setExplorerSectionCollapsed = useShellStore((s) => s.setExplorerSectionCollapsed);

	const [switcherOpen, setSwitcherOpen] = useState(false);
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

	const active = projects.find((p) => p.id === activeProjectId);
	const projectName = active?.display_name ?? (activeProject?.root_path?.split('/').pop() || 'No Project');
	const switchHint = labelFor('palette.projects');

	const sorted = projects.slice().sort((a, b) => {
		if (a.id === activeProjectId) return -1;
		if (b.id === activeProjectId) return 1;
		const aArc = a.archived_at != null ? 1 : 0;
		const bArc = b.archived_at != null ? 1 : 0;
		if (aArc !== bArc) return aArc - bArc;
		if (a.position !== b.position) return a.position - b.position;
		return a.created_at - b.created_at;
	});

	async function pickProject(id: string) {
		setSwitcherOpen(false);
		try {
			await setActiveProject(id);
		} catch {
			// Handled by store
		}
	}

	function openProjectsSettings() {
		setSwitcherOpen(false);
		usePaneStore.getState().navigateFocused('/settings/projects');
	}

	return (
		<div className="flex shrink-0 items-center justify-between px-3 py-2 border-b border-border bg-background">
			<Popover open={switcherOpen} onOpenChange={setSwitcherOpen}>
				<PopoverTrigger asChild>
					<button
						type="button"
						data-testid="explorer-project-chip"
						aria-label={`Project: ${projectName} — switch project (${switchHint})`}
						title={`Project: ${projectName} (${switchHint})`}
						className="flex items-center gap-1.5 text-[11px] font-semibold text-foreground hover:bg-accent hover:text-accent-foreground px-2 py-1 rounded cursor-pointer truncate max-w-[200px] min-h-[24px] focus-visible:ring-2 focus-visible:ring-ring focus:outline-none"
					>
						<Folder className="h-3 w-3 shrink-0 text-muted-foreground" />
						<span className="truncate">{projectName}</span>
						<ChevronDown className="h-3 w-3 shrink-0 text-muted-foreground opacity-60" />
					</button>
				</PopoverTrigger>
				<PopoverContent side="bottom" align="start" className="w-64 p-2">
					<div className="flex items-center justify-between px-2 pb-2 pt-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
						<span>Switch project</span>
						<kbd className="font-mono text-[9px] text-muted-foreground">{switchHint}</kbd>
					</div>
					<ul className="flex max-h-72 flex-col overflow-y-auto">
						{sorted.map((p) => (
							<li key={p.id}>
								<button
									type="button"
									onClick={() => void pickProject(p.id)}
									aria-current={p.id === activeProjectId ? 'true' : undefined}
									className={cn(
										'flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs outline-none transition-colors motion-reduce:transition-none',
										'hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset',
										p.id === activeProjectId && 'bg-accent/60 font-medium',
										p.archived_at != null && 'opacity-60'
									)}
								>
									<span
										aria-hidden
										className="inline-block h-2.5 w-2.5 shrink-0 rounded-full border border-border"
										style={{ background: p.color ?? 'var(--fg-faint)' }}
									/>
									{p.icon && <span className="text-xs leading-none">{p.icon}</span>}
									<span className="flex-1 truncate">{p.display_name}</span>
									{p.id === activeProjectId && (
										<span className="text-[10px] text-muted-foreground">Active</span>
									)}
								</button>
							</li>
						))}
						{sorted.length === 0 && (
							<li className="px-2 py-3 text-center text-xs text-muted-foreground">
								No projects found
							</li>
						)}
					</ul>
					<div className="mt-1 border-t border-border pt-1">
						<button
							type="button"
							onClick={openProjectsSettings}
							className="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-xs text-muted-foreground hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
						>
							<Plus className="h-3 w-3" />
							<span>Manage projects…</span>
						</button>
					</div>
				</PopoverContent>
			</Popover>

			<div className="flex items-center gap-1">
				<button
					type="button"
					onClick={handleCollapseToggle}
					className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-accent-foreground min-h-[24px] min-w-[24px] flex items-center justify-center focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring focus:outline-none"
					title={allCollapsed ? 'Restore sections' : 'Collapse all sections'}
					aria-label={allCollapsed ? 'Restore sections' : 'Collapse all sections'}
				>
					{allCollapsed ? (
						<FoldVertical className="h-4 w-4" />
					) : (
						<ChevronsDownUp className="h-4 w-4" />
					)}
				</button>
			</div>
		</div>
	);
}
