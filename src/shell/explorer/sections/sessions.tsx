import { useCallback } from 'react';
import { LayoutGrid, TerminalSquare } from 'lucide-react';
import { ListRow } from '@/components/ui/list-row';
import { cn } from '@/components/ui/utils';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useShellStore } from '@/lib/shell/shell-store';
import { useTerminalStore } from '@/terminal/session-store';
import { createTerminalSession } from '@/terminal/single-terminal';
import { EmptyState } from '@/components/states';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import { handToChi } from '@/shell/companion/companion-store';
import { boardIsShowing, openBoard } from '@/shell/chi-board/board-store';
import type { ExplorerSectionContext } from '../section-registry';
import { SectionErrorRow } from '../section-error-row';

/**
 * WP-68 (D-09): the Sessions header's "Seats" link to the seat board (`/chi`,
 * the focused pane, one tab). `aria-current="page"` while the board is the
 * active tab of a pane.
 *
 * WP-71a: it sits in the section's header row, as D-09 LISTING draws it
 * (`seats-board.html` `.seclink`), through the registry's `headerActions`
 * slot (`section-registry.ts`) rather than at the head of the body. The
 * frame renders it beside the header button, never inside it, so clicking
 * it never collapses the section.
 */
export function SessionsSeatsLink(_ctx: ExplorerSectionContext) {
	const onBoard = usePaneStore((s) => boardIsShowing(s.root));
	return (
		<button
			type="button"
			data-explorer-seats-link=""
			onClick={openBoard}
			aria-current={onBoard ? 'page' : undefined}
			title="All seats — open the seat board (/chi) in the focused pane"
			className={cn(
				'inline-flex h-5 items-center gap-1 rounded-sm border bg-card px-2 text-[11px] text-muted-foreground @max-[15rem]/sechead:px-1',
				'hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring',
				// D-09 `.seclink[aria-current='page']` outranks its `:hover`.
				onBoard ? 'border-primary text-foreground' : 'border-border-soft hover:border-[var(--border-strong)]'
			)}
		>
			<LayoutGrid className="h-3 w-3" aria-hidden="true" />
			{/* Icon-only in a narrow sidebar (section-frame's `sechead` container). */}
			<span className="@max-[15rem]/sechead:sr-only">Seats</span>
		</button>
	);
}

export function SessionsSection(_ctx: ExplorerSectionContext) {
	const tabs = useTerminalStore((s) => s.tabs);
	const restoreError = useTerminalStore((s) => s.restoreError);
	const dismissRestoreError = useTerminalStore((s) => s.dismissRestoreError);

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

	// A failed restore is not "no sessions": say so above whatever is shown.
	const restoreRow = restoreError ? (
		<SectionErrorRow
			message={restoreError.message}
			onRetry={dismissRestoreError}
			actionLabel={restoreError.holdsSave ? 'Resume saving' : 'Dismiss'}
			// The "saving resumed, copied to …" notice reports a success.
			tone={restoreError.backupKey ? 'info' : 'error'}
		/>
	) : null;

	if (tabs.length === 0 && restoreRow) {
		return <div className="py-1">{restoreRow}</div>;
	}

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
			{restoreRow}
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
						// A-9: `rename` (it needed `window.prompt`; no dialog-shim
						// prompt exists) and `kill-session` (removing the tab entry
						// doesn't kill its PTY, and there is no tab→PTY kill API) are
						// left out until they have real handlers.
						builtinsNeedHandler
						handlers={{
							open: () => openSession(tab.id),
							'open-to-side': () => openSessionSplit(tab.id),
							'make-dispatch': () =>
								useShellStore.getState().setCompanionTarget({ kind: 'session', session_id: tab.id }),
							'hand-to-chi': () => handToChi(tab.title || tab.id),
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
