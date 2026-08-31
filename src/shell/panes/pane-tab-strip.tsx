import { useMemo, useRef, useState } from 'react';
import { Plus } from 'lucide-react';
import { writeClipboardText } from '@/lib/transport/shims';
import type { LeafNode } from '@/lib/panes/types';
import { usePaneStore } from '@/lib/panes/pane-store';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { useDragState } from '@/lib/panes/drag-state';
import { TabStrip, Tab } from '@/components/ui/tab-strip';
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuSeparator,
	ContextMenuTrigger,
} from '@/components/ui/context-menu';
import { useTerminalTitles } from '@/terminal/use-terminal-titles';
import { viewLabel, viewSubtitle } from './pane-views';
import { viewWorkspace } from './tab-workspace';
import { NewTabMenu, useAnchorRect } from './new-tab-menu';
import { PinArtifactDialog } from './pin-artifact-dialog';
import { cn } from '@/components/ui/utils';

interface PaneTabStripProps {
	leaf: LeafNode;
	isFocused: boolean;
}

export function PaneTabStrip({ leaf, isFocused }: PaneTabStripProps) {
	const switchTab = usePaneStore((s) => s.switchTab);
	const closeTab = usePaneStore((s) => s.closeTab);
	const focusPane = usePaneStore((s) => s.focusPane);
	const toggleTabPinned = usePaneStore((s) => s.toggleTabPinned);
	const reorderTab = usePaneStore((s) => s.reorderTab);
	const placeView = usePaneStore((s) => s.placeView);

	// Path of the artifact being pinned to the sidebar via the tab context menu
	// (null = dialog closed). Reuses the same PinArtifactDialog as the address bar.
	const [pinPath, setPinPath] = useState<string | null>(null);

	// Names terminal tabs by what they're running and where — `claude · shell`
	// rather than N tabs all reading "Terminal".
	const resolveTerminal = useTerminalTitles();

	// Close every closable (non-pinned) tab except `keepIdx`. Read fresh state
	// and close in DESCENDING index order so earlier closes never shift the
	// indices of the ones still to close.
	function closeOthers(keepIdx: number) {
		const lf = findLeaf(usePaneStore.getState().root, leaf.id);
		if (!lf) return;
		for (let i = lf.tabs.length - 1; i >= 0; i--) {
			if (i === keepIdx || lf.tabs[i].pinned) continue;
			closeTab(leaf.id, i);
		}
	}
	function closeToRight(fromIdx: number) {
		const lf = findLeaf(usePaneStore.getState().root, leaf.id);
		if (!lf) return;
		for (let i = lf.tabs.length - 1; i > fromIdx; i--) {
			if (lf.tabs[i].pinned) continue;
			closeTab(leaf.id, i);
		}
	}

	const [menuOpen, setMenuOpen] = useState(false);
	const addBtnRef = useRef<HTMLButtonElement | null>(null);
	const anchor = useAnchorRect(menuOpen, addBtnRef);

	// Drop indicator for in-strip reorder: { idx, side }. `side='before'`
	// means the drop will insert *before* tab idx; `'after'` means after.
	const [dropAt, setDropAt] = useState<{ idx: number; side: 'before' | 'after' } | null>(null);

	// Per `<workspace>/design/shell/concepts/_shared/shell.css` §"Workspace tint on tabs":
	// single-workspace strips suppress inactive hairlines (the pane focus accent
	// already announces the workspace). Mixed strips opt in to the per-tab tint
	// hairline so inactive tabs each show their own workspace at low alpha.
	const isMixedWorkspace = useMemo(() => {
		if (leaf.tabs.length < 2) return false;
		const ws = leaf.tabs.map(viewWorkspace);
		return ws.some((w) => w !== ws[0]);
	}, [leaf.tabs]);

	function activate(idx: number) {
		focusPane(leaf.id);
		switchTab(leaf.id, idx);
	}

	function handleAddClick() {
		focusPane(leaf.id);
		setMenuOpen((v) => !v);
	}

	return (
		<div
			className={cn(
				'flex h-8 shrink-0 items-stretch border-b border-border bg-card',
				isFocused ? 'opacity-100' : 'opacity-80'
			)}
		>
			<TabStrip
				label="Open tabs"
				className="flex-1"
				activeIdx={leaf.activeTabIdx}
				count={leaf.tabs.length}
				onSwitch={activate}
				onReorder={(from, to) => reorderTab(leaf.id, from, to)}
				mixed={isMixedWorkspace}
			>
				{leaf.tabs.map((tab, idx) => {
					const isActive = idx === leaf.activeTabIdx;
					const isPinned = Boolean(tab.pinned);
					const ws = viewWorkspace(tab);
					const label = viewLabel(tab, resolveTerminal);
					return (
						<ContextMenu key={`${idx}-${tab.kind}`}>
							<ContextMenuTrigger asChild>
								<Tab
									index={idx}
									active={isActive}
									ws={ws}
									label={label}
									// Terminal labels are real command + directory names
									// (`claude · shell`). Title-casing them would render
									// "Claude · Shell" and misspell anything lowercase by
									// convention; route labels still capitalize.
									labelClassName={tab.kind === 'terminal' ? undefined : 'capitalize'}
									title={`${label}${isPinned ? ' (pinned)' : ''}\n${viewSubtitle(tab, resolveTerminal)}`}
									pinned={isPinned}
									closable={!isPinned}
									onActivate={() => activate(idx)}
									onClose={() => closeTab(leaf.id, idx)}
									onTogglePin={() => toggleTabPinned(leaf.id, idx)}
									onMiddleClick={!isPinned ? () => closeTab(leaf.id, idx) : undefined}
									draggable={!isPinned}
									dropEdge={dropAt?.idx === idx ? dropAt.side : null}
									className={cn(
										'border-r border-border',
										isPinned
											? 'min-w-[32px] max-w-[140px] px-2'
											: 'min-w-[120px] max-w-[180px] px-3'
									)}
									dragHandlers={{
										onDragStart: (e) => {
											if (isPinned) {
												e.preventDefault();
												return;
											}
											e.dataTransfer.effectAllowed = 'move';
											// Some browsers cancel the drag unless dataTransfer carries
											// data — the real payload lives in useDragState.
											e.dataTransfer.setData('application/x-pane-tab', `${leaf.id}:${idx}`);
											useDragState.getState().startPane(leaf.id, idx);
										},
										onDragEnd: () => {
											useDragState.getState().end();
											setDropAt(null);
										},
										onDragOver: (e) => {
											const drag = useDragState.getState();
											if (drag.source !== 'pane' || drag.srcLeafId !== leaf.id) return;
											if (drag.srcTabIdx === idx) return;
											e.preventDefault();
											e.dataTransfer.dropEffect = 'move';
											const rect = e.currentTarget.getBoundingClientRect();
											const side = e.clientX < rect.left + rect.width / 2 ? 'before' : 'after';
											setDropAt((prev) =>
												prev && prev.idx === idx && prev.side === side ? prev : { idx, side }
											);
										},
										onDragLeave: (e) => {
											// Only clear when leaving the tab entirely, not on child enter.
											if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
											setDropAt((prev) => (prev?.idx === idx ? null : prev));
										},
										onDrop: (e) => {
											const drag = useDragState.getState();
											if (drag.source !== 'pane' || drag.srcLeafId !== leaf.id) return;
											if (drag.srcTabIdx === null || drag.srcTabIdx === idx) {
												setDropAt(null);
												return;
											}
											e.preventDefault();
											e.stopPropagation();
											const rect = e.currentTarget.getBoundingClientRect();
											const side = e.clientX < rect.left + rect.width / 2 ? 'before' : 'after';
											const from = drag.srcTabIdx;
											// Compute destination index in the *current* tabs array.
											let to = side === 'before' ? idx : idx + 1;
											if (from < to) to -= 1;
											reorderTab(leaf.id, from, to);
											setDropAt(null);
											drag.end();
										},
									}}
								/>
							</ContextMenuTrigger>
							<ContextMenuContent>
								{tab.kind === 'artifact' && (
									<>
										<ContextMenuItem onSelect={() => setPinPath(tab.path)}>
											Pin to sidebar…
										</ContextMenuItem>
										<ContextMenuSeparator />
									</>
								)}
								<ContextMenuItem onSelect={() => toggleTabPinned(leaf.id, idx)}>
									{isPinned ? 'Unpin tab' : 'Pin tab'}
								</ContextMenuItem>
								<ContextMenuSeparator />
								<ContextMenuItem onSelect={() => placeView(leaf.id, tab, 'right')}>
									Split right
								</ContextMenuItem>
								<ContextMenuItem onSelect={() => placeView(leaf.id, tab, 'bottom')}>
									Split down
								</ContextMenuItem>
								{(tab.kind === 'artifact' || tab.kind === 'route') && (
									<>
										<ContextMenuSeparator />
										<ContextMenuItem onSelect={() => void writeClipboardText(tab.path).catch(() => {})}>
											Copy path
										</ContextMenuItem>
									</>
								)}
								<ContextMenuSeparator />
								<ContextMenuItem disabled={isPinned} onSelect={() => closeTab(leaf.id, idx)}>
									Close
								</ContextMenuItem>
								<ContextMenuItem onSelect={() => closeOthers(idx)}>Close others</ContextMenuItem>
								<ContextMenuItem onSelect={() => closeToRight(idx)}>
									Close to the right
								</ContextMenuItem>
							</ContextMenuContent>
						</ContextMenu>
					);
				})}
			</TabStrip>
			<button
				ref={addBtnRef}
				type="button"
				onClick={(e) => {
					e.stopPropagation();
					handleAddClick();
				}}
				title="New tab in pane"
				aria-label="New tab"
				aria-expanded={menuOpen}
				className="flex h-full w-8 items-center justify-center text-muted-foreground hover:bg-accent hover:text-accent-foreground"
			>
				<Plus className="h-3.5 w-3.5" />
			</button>
			<NewTabMenu leaf={leaf} open={menuOpen} onClose={() => setMenuOpen(false)} anchor={anchor} />
			{pinPath !== null && (
				<PinArtifactDialog
					open
					onOpenChange={(o) => {
						if (!o) setPinPath(null);
					}}
					path={pinPath}
					onPinned={() => setPinPath(null)}
				/>
			)}
		</div>
	);
}
