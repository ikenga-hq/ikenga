import { useMemo, useRef, useState } from 'react';
import { Plus } from 'lucide-react';
import { writeClipboardText } from '@/lib/transport';
import type { LeafNode } from '@/lib/panes/types';
import { usePaneStore } from '@/lib/panes/pane-store';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { useDragState } from '@/lib/panes/drag-state';
import { beginPointerDrag, useDropTarget } from '@/lib/panes/pointer-drag';
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

	// The slot under client `x`: which tab, and which half of it. Past the last
	// tab (empty strip space) counts as after the last one.
	function slotAt(stripEl: Element, x: number): { idx: number; side: 'before' | 'after' } | null {
		const tabEls = Array.from(
			stripEl.querySelectorAll<HTMLElement>('[role="tab"][data-tab-index]')
		);
		if (tabEls.length === 0) return null;
		for (const el of tabEls) {
			const r = el.getBoundingClientRect();
			if (x < r.left && el === tabEls[0])
				return { idx: Number(el.dataset.tabIndex), side: 'before' };
			if (x >= r.left && x <= r.right) {
				return {
					idx: Number(el.dataset.tabIndex),
					side: x < r.left + r.width / 2 ? 'before' : 'after',
				};
			}
		}
		return { idx: Number(tabEls[tabEls.length - 1].dataset.tabIndex), side: 'after' };
	}

	// In-strip reorder: one pointer-drag target for the whole strip. Only this
	// pane's own tabs reorder here; moves between panes go through the body
	// drop zones.
	const stripDrop = useDropTarget({
		accepts: () => {
			const d = useDragState.getState();
			return d.active && d.source === 'pane' && d.srcLeafId === leaf.id;
		},
		onOver: (x, _y, el) => {
			const slot = slotAt(el, x);
			if (!slot || slot.idx === useDragState.getState().srcTabIdx) {
				setDropAt(null);
				return;
			}
			setDropAt((prev) => (prev && prev.idx === slot.idx && prev.side === slot.side ? prev : slot));
		},
		onLeave: () => setDropAt(null),
		onDrop: (x, _y, el) => {
			setDropAt(null);
			const d = useDragState.getState();
			const from = d.srcTabIdx;
			const slot = slotAt(el, x);
			if (slot && from != null && slot.idx !== from) {
				// Destination index in the *current* tabs array.
				let to = slot.side === 'before' ? slot.idx : slot.idx + 1;
				if (from < to) to -= 1;
				if (to !== from) reorderTab(leaf.id, from, to);
			}
			d.end();
		},
	});

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
				dropTarget={stripDrop}
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
									dropEdge={dropAt?.idx === idx ? dropAt.side : null}
									className={cn(
										'border-r border-border',
										isPinned
											? 'min-w-[32px] max-w-[140px] px-2'
											: 'min-w-[120px] max-w-[180px] px-3'
									)}
									onDragPointerDown={
										isPinned
											? undefined
											: (e) =>
													beginPointerDrag(e, {
														label,
														onStart: () => useDragState.getState().startPane(leaf.id, idx),
														onEnd: () => {
															useDragState.getState().end();
															setDropAt(null);
														},
													})
									}
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
										<ContextMenuItem
											onSelect={() => void writeClipboardText(tab.path).catch(() => {})}
										>
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
