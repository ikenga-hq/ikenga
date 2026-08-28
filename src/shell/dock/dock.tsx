import { useEffect, useRef, useState } from 'react';
import { ChevronLeft, ChevronRight, Plus, Terminal as TerminalIcon } from 'lucide-react';
import type { PaneView } from '@/lib/panes/types';
import { CommandRow } from '@/components/ui/command-row';
import { FeedbackState } from '@/components/ui/feedback-state';
import { IconButton } from '@/components/ui/icon-button';
import { TabStrip, Tab, TabRail, RailTab } from '@/components/ui/tab-strip';
import { useDockStore, DOCK_MIN_WIDTH, DOCK_MAX_WIDTH } from './dock-store';
import { useDragState } from '@/lib/panes/drag-state';
import { usePaneStore } from '@/lib/panes/pane-store';
import { PaneBody, viewLabel, viewSubtitle } from '@/shell/panes/pane-views';
import { useTerminalTitles } from '@/terminal/use-terminal-titles';
import { viewKey } from '@/shell/panes/view-key';
import { viewWorkspace } from '@/shell/panes/tab-workspace';
import { createClaudeTerminalSession, createTerminalSession } from '@/terminal/single-terminal';
import { cn } from '@/components/ui/utils';

const COLLAPSED_WIDTH = '36px';

export function Dock() {
	const dockState = useDockStore((s) => s.state);
	const tabs = useDockStore((s) => s.tabs);
	const activeIdx = useDockStore((s) => s.activeIdx);
	const setState = useDockStore((s) => s.setState);
	const switchTab = useDockStore((s) => s.switchTab);
	const closeTab = useDockStore((s) => s.closeTab);
	const togglePinned = useDockStore((s) => s.togglePinned);
	const addTab = useDockStore((s) => s.addTab);
	const appendView = useDockStore((s) => s.appendView);
	const storedWidth = useDockStore((s) => s.width);
	const setStoredWidth = useDockStore((s) => s.setWidth);
	// Terminal tabs name themselves by running command + directory.
	const resolveTerminal = useTerminalTitles();

	const drag = useDragState();
	const [dropHover, setDropHover] = useState(false);

	if (dockState === 'hidden') return null;

	const width = dockState === 'collapsed' ? COLLAPSED_WIDTH : `${storedWidth}px`;

	// Pane → dock: detach the source tab and append it as a dock tab. We use
	// moveTab to a sentinel pane id won't work, so instead we read the source
	// view directly off the pane store and explicitly closeTab there. Dock →
	// dock drops are no-ops for now (in-dock reordering is out of scope).
	function handleExternalDrop() {
		setDropHover(false);
		if (
			!drag.active ||
			drag.source !== 'pane' ||
			drag.srcLeafId == null ||
			drag.srcTabIdx == null
		) {
			drag.end();
			return;
		}
		const paneStore = usePaneStore.getState();
		const root = paneStore.root;
		const srcLeaf = findLeafShallow(root, drag.srcLeafId);
		if (!srcLeaf) {
			drag.end();
			return;
		}
		const view = srcLeaf.tabs[drag.srcTabIdx];
		if (!view) {
			drag.end();
			return;
		}
		// Append into dock first, then close from source pane.
		appendView(view);
		paneStore.closeTab(drag.srcLeafId, drag.srcTabIdx);
		drag.end();
	}

	if (dockState === 'collapsed') {
		return (
			<aside
				aria-label="Dock"
				className="flex h-full flex-col border-l py-3"
				style={{
					width,
					background: 'var(--bg-base)',
					borderColor: 'var(--border-soft)',
				}}
				onDragOver={(e) => {
					if (drag.active) {
						e.preventDefault();
						e.dataTransfer.dropEffect = 'move';
					}
				}}
				onDrop={handleExternalDrop}
			>
				<TabRail
					label="Dock tabs"
					className="px-1"
					activeIdx={activeIdx}
					count={tabs.length}
					onSwitch={switchTab}
				>
					{tabs.map((tab, idx) => {
						const ws = viewWorkspace(tab);
						const isActive = idx === activeIdx;
						const isPinned = Boolean(tab.pinned);
						return (
							<RailTab
								key={`${idx}-${tab.kind}`}
								index={idx}
								active={isActive}
								ws={ws}
								label={viewLabel(tab, resolveTerminal)}
								glyph={<DockTabIcon view={tab} />}
								onActivate={() => {
									switchTab(idx);
									setState('expanded');
								}}
								draggable={!isPinned}
								dragHandlers={{
									onDragStart: (e) => {
										if (isPinned) {
											e.preventDefault();
											return;
										}
										e.dataTransfer.effectAllowed = 'move';
										e.dataTransfer.setData('application/x-dock-tab', `${idx}`);
										useDragState.getState().startDock(idx);
									},
									onDragEnd: () => useDragState.getState().end(),
								}}
							/>
						);
					})}
				</TabRail>
				<button
					type="button"
					onClick={() => setState('expanded')}
					title="Expand dock"
					aria-label="Expand dock"
					className="mx-auto mt-1 grid size-7 place-items-center rounded-sm text-muted-foreground hover:bg-card"
				>
					<ChevronLeft className="h-3.5 w-3.5" />
				</button>
			</aside>
		);
	}

	// expanded
	const activeTab = tabs[activeIdx];
	return (
		<aside
			aria-label="Dock"
			className="relative flex h-full flex-col border-l"
			style={{
				width,
				background: 'var(--bg-base)',
				borderColor: 'var(--border-soft)',
			}}
		>
			<DockResizeHandle width={storedWidth} setWidth={setStoredWidth} />
			<div
				className="flex shrink-0 items-stretch border-b"
				style={{
					height: 'var(--tab-h)',
					borderColor: 'var(--border-soft)',
					background: 'var(--bg-sunken)',
				}}
				onDragOver={(e) => {
					if (drag.active) {
						e.preventDefault();
						e.dataTransfer.dropEffect = 'move';
						setDropHover(true);
					}
				}}
				onDragLeave={() => setDropHover(false)}
				onDrop={handleExternalDrop}
			>
				<TabStrip
					label="Dock tabs"
					className={cn('flex-1 gap-1 px-2', dropHover && 'bg-primary/10')}
					activeIdx={activeIdx}
					count={tabs.length}
					onSwitch={switchTab}
					mixed={tabs.length > 1}
				>
					{tabs.map((tab, idx) => {
						const ws = viewWorkspace(tab);
						const isActive = idx === activeIdx;
						const isPinned = Boolean(tab.pinned);
						return (
							<Tab
								key={`${idx}-${tab.kind}`}
								index={idx}
								active={isActive}
								ws={ws}
								glyph={<DockTabIcon view={tab} />}
								label={viewLabel(tab, resolveTerminal)}
								// See pane-tab-strip: a terminal label is literal command
								// and directory names, so it must not be title-cased.
								labelClassName={tab.kind === 'terminal' ? undefined : 'capitalize'}
								title={`${viewLabel(tab, resolveTerminal)}\n${viewSubtitle(tab, resolveTerminal)}`}
								className="px-3"
								pinned={isPinned}
								closable={!isPinned}
								onActivate={() => switchTab(idx)}
								onClose={() => closeTab(idx)}
								onTogglePin={() => togglePinned(idx)}
								onMiddleClick={!isPinned ? () => closeTab(idx) : undefined}
								draggable={!isPinned}
								dragHandlers={{
									onDragStart: (e) => {
										if (isPinned) {
											e.preventDefault();
											return;
										}
										e.dataTransfer.effectAllowed = 'move';
										e.dataTransfer.setData('application/x-dock-tab', `${idx}`);
										useDragState.getState().startDock(idx);
									},
									onDragEnd: () => useDragState.getState().end(),
								}}
							/>
						);
					})}
				</TabStrip>
				<div
					className="flex shrink-0 items-center gap-1 border-l px-1"
					style={{ borderColor: 'var(--border-soft)' }}
				>
					<DockAddButton onAdd={addTab} />
					<IconButton
						onClick={() => setState('collapsed')}
						title="Collapse dock"
						aria-label="Collapse dock"
					>
						<ChevronRight className="h-3.5 w-3.5" />
					</IconButton>
				</div>
			</div>
			<div className="relative flex-1 overflow-hidden" style={{ background: 'var(--bg-base)' }}>
				{activeTab ? (
					<PaneBody key={viewKey(activeTab)} paneId="__dock__" view={activeTab} />
				) : (
					<DockEmpty
						onSeedTerminal={() => {
							appendView({ kind: 'terminal', sessionId: createTerminalSession() });
						}}
					/>
				)}
				<div
					aria-hidden="true"
					onDragEnter={(e) => {
						if (drag.active && drag.source === 'pane') e.preventDefault();
					}}
					onDragOver={(e) => {
						if (!drag.active || drag.source !== 'pane') return;
						e.preventDefault();
						e.dataTransfer.dropEffect = 'move';
						setDropHover(true);
					}}
					onDragLeave={() => setDropHover(false)}
					onDrop={handleExternalDrop}
					className={cn(
						'absolute inset-0 z-20 flex items-center justify-center border-2 border-dashed text-xs font-medium transition-colors',
						drag.active && drag.source === 'pane'
							? 'pointer-events-auto'
							: 'pointer-events-none opacity-0',
						dropHover
							? 'border-primary bg-primary/15 text-primary'
							: 'border-primary/40 bg-background/60 text-muted-foreground'
					)}
				>
					Drop to dock
				</div>
			</div>
		</aside>
	);
}

function DockResizeHandle({ width, setWidth }: { width: number; setWidth: (n: number) => void }) {
	const startRef = useRef<{ x: number; w: number } | null>(null);

	function onPointerDown(e: React.PointerEvent<HTMLDivElement>) {
		e.preventDefault();
		(e.currentTarget as HTMLDivElement).setPointerCapture(e.pointerId);
		startRef.current = { x: e.clientX, w: width };
		document.body.style.cursor = 'col-resize';
		document.body.style.userSelect = 'none';
	}

	function onPointerMove(e: React.PointerEvent<HTMLDivElement>) {
		if (!startRef.current) return;
		const dx = e.clientX - startRef.current.x;
		// Dock is on the right edge — dragging left grows it.
		const next = startRef.current.w - dx;
		setWidth(Math.max(DOCK_MIN_WIDTH, Math.min(DOCK_MAX_WIDTH, next)));
	}

	function onPointerUp(e: React.PointerEvent<HTMLDivElement>) {
		startRef.current = null;
		try {
			(e.currentTarget as HTMLDivElement).releasePointerCapture(e.pointerId);
		} catch {
			// ignore
		}
		document.body.style.cursor = '';
		document.body.style.userSelect = '';
	}

	return (
		<div
			role="separator"
			aria-orientation="vertical"
			aria-label="Resize dock"
			onPointerDown={onPointerDown}
			onPointerMove={onPointerMove}
			onPointerUp={onPointerUp}
			onPointerCancel={onPointerUp}
			className="absolute left-0 top-0 z-10 h-full w-1 -translate-x-1/2 cursor-col-resize hover:bg-primary/30"
		/>
	);
}

function DockTabIcon({ view }: { view: PaneView }) {
	switch (view.kind) {
		case 'terminal':
			return <TerminalIcon className="h-3.5 w-3.5" />;
		default:
			return <span className="h-3.5 w-3.5" aria-hidden="true" />;
	}
}

function DockAddButton({ onAdd }: { onAdd: (view: PaneView) => void }) {
	const [open, setOpen] = useState(false);
	const btnRef = useRef<HTMLButtonElement | null>(null);
	const menuRef = useRef<HTMLDivElement | null>(null);

	useEffect(() => {
		if (!open) return;
		const onDown = (e: MouseEvent) => {
			const t = e.target as Node | null;
			if (t && !btnRef.current?.contains(t) && !menuRef.current?.contains(t)) {
				setOpen(false);
			}
		};
		const onKey = (e: KeyboardEvent) => {
			if (e.key === 'Escape') setOpen(false);
		};
		window.addEventListener('mousedown', onDown);
		window.addEventListener('keydown', onKey);
		return () => {
			window.removeEventListener('mousedown', onDown);
			window.removeEventListener('keydown', onKey);
		};
	}, [open]);

	function pick(view: PaneView) {
		onAdd(view);
		setOpen(false);
	}

	return (
		<div className="relative">
			<IconButton
				ref={btnRef}
				onClick={() => setOpen((v) => !v)}
				title="New tab"
				aria-label="New tab"
				aria-haspopup="menu"
				aria-expanded={open}
			>
				<Plus className="h-3.5 w-3.5" />
			</IconButton>
			{open && (
				<div
					ref={menuRef}
					role="menu"
					className="absolute right-0 top-full z-50 mt-1 w-44 overflow-hidden rounded-md border border-border bg-popover py-1 text-popover-foreground shadow-lg"
				>
					<DockMenuItem
						Icon={TerminalIcon}
						label="New terminal"
						onClick={() => pick({ kind: 'terminal', sessionId: createTerminalSession() })}
					/>
					<DockMenuItem
						Icon={TerminalIcon}
						label="New Claude terminal"
						onClick={() =>
							pick({
								kind: 'terminal',
								sessionId: createClaudeTerminalSession(),
							})
						}
					/>
				</div>
			)}
		</div>
	);
}

// Dock `+` menu rows share the consolidated `CommandRow` (size `sm`, rendered
// as a `<button role="menuitem">` for the `role="menu"` container). This gains
// the focus-visible ring the hand-rolled button lacked; the dropdown's keyboard
// roving-tabindex remains a known dock-level gap (see command-row.md §4).
function DockMenuItem({
	Icon,
	label,
	onClick,
}: {
	Icon: typeof Plus;
	label: string;
	onClick: () => void;
}) {
	return <CommandRow size="sm" as="menuitem" Icon={Icon} label={label} onSelect={onClick} />;
}

function DockEmpty({ onSeedTerminal }: { onSeedTerminal: () => void }) {
	return (
		<FeedbackState
			variant="empty"
			fill
			heading="The dock is empty."
			body={
				<>
					Drag tabs in from any pane, or
					<br />
					seed a new session.
				</>
			}
			action={
				<>
					<button
						type="button"
						onClick={onSeedTerminal}
						className="rounded border px-3 py-1 text-xs hover:bg-card"
						style={{ borderColor: 'var(--border)' }}
					>
						New terminal
					</button>
				</>
			}
		/>
	);
}

// Light helper — same shape as pane-reducer's findLeaf, kept inline so the
// dock doesn't import internal pane-store machinery directly.
function findLeafShallow(
	node: import('@/lib/panes/types').PaneNode,
	id: string
): import('@/lib/panes/types').LeafNode | null {
	if (node.type === 'leaf') return node.id === id ? node : null;
	for (const child of node.children) {
		const found = findLeafShallow(child, id);
		if (found) return found;
	}
	return null;
}
