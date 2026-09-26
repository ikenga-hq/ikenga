import { useCallback } from 'react';
import { TerminalSquare } from 'lucide-react';
import { ListRow } from '@/components/ui/list-row';
import { cn } from '@/components/ui/utils';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useShellStore } from '@/lib/shell/shell-store';
import { useTerminalStore } from '@/terminal/session-store';
import { createTerminalSession } from '@/terminal/single-terminal';
import { EmptyState } from '@/components/states';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import { handToChi } from '@/shell/companion/companion-store';
import type { ExplorerSectionContext } from '../section-registry';

// WP-04 stub array — real menu content is `getEffectiveMenu('session')`
// below (G-ACTIONS §1.3). Kept for `section-registry.ts`'s unused
// `contextMenu` field (out of this WP's FILES list; see the PR report).
export const sessionsContextMenu = [
	{ id: 'open-pane', label: 'Open in pane', run: () => {} },
	{ id: 'open-side', label: 'Open to the Side', run: () => {} },
	{ id: 'make-dispatch', label: 'Make dispatch target', run: () => {} },
	{ id: 'hand-to-chi', label: 'Hand to Chi', run: () => {} },
	{ id: 'rename', label: 'Rename…', run: () => {} },
	{ id: 'kill', label: 'Kill session', run: () => {} },
];

export function SessionsSection(_ctx: ExplorerSectionContext) {
	const tabs = useTerminalStore((s) => s.tabs);
	const renameTab = useTerminalStore((s) => s.rename);
	const removeTab = useTerminalStore((s) => s.remove);

	const openSession = useCallback((sessionId: string) => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'terminal', sessionId });
	}, []);

	const openSessionSplit = useCallback((sessionId: string) => {
		const { focusedId, placeView } = usePaneStore.getState();
		placeView(focusedId, { kind: 'terminal', sessionId }, 'right');
	}, []);

	const handleStartSession = useCallback(() => {
		const sessionId = createTerminalSession();
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'terminal', sessionId });
	}, []);

	if (tabs.length === 0) {
		return (
			<EmptyState
				data-state="explorer-sessions-empty"
				icon={TerminalSquare}
				heading="No sessions in this project"
				body="A session is a terminal with an engine in it. Starting one also starts the cost and tool feed."
				action={{ label: 'Start a session', onClick: handleStartSession }}
			/>
		);
	}

	return (
		<div className="py-1">
			{tabs.map((tab) => {
				const isRunning = tab.status === 'running';
				const isSpawning = tab.status === 'spawning';
				const pulseClass = isRunning
					? 'bg-emerald-500'
					: isSpawning
					? 'bg-amber-500 animate-pulse'
					: 'bg-muted-foreground/40';

				return (
					<EffectiveContextMenu
						key={tab.id}
						menuId="session"
						handlers={{
							open: () => openSession(tab.id),
							'open-to-side': () => openSessionSplit(tab.id),
							'make-dispatch': () =>
								useShellStore.getState().setCompanionTarget({ kind: 'session', session_id: tab.id }),
							'hand-to-chi': () => handToChi(tab.title || tab.id),
							rename: () => {
								const next = window.prompt('Rename session', tab.title || tab.id);
								if (next && next.trim()) renameTab(tab.id, next.trim());
							},
							'kill-session': () => removeTab(tab.id),
						}}
					>
						<ListRow
							size="sm"
							onActivate={() => openSession(tab.id)}
							title={tab.title || tab.id}
							className="w-full gap-1.5 px-2"
						>
							<span className={cn('size-1.5 shrink-0 rounded-full', pulseClass)} aria-hidden="true" />
							<span className="flex-1 truncate text-xs">{tab.title || `Terminal (${tab.id.slice(0, 6)})`}</span>
							<span className="text-[10px] text-muted-foreground font-mono">
								{tab.spec.cmd?.[0] || 'sh'}
							</span>
						</ListRow>
					</EffectiveContextMenu>
				);
			})}
		</div>
	);
}
