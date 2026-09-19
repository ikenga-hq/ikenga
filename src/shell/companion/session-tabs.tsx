// Session tabs — spec §3.11 #66–#67. One tab per session parked in the
// Companion. Selecting a tab sets BOTH the panel scope and
// `companion.activeTarget`; it never starts or stops a run.
//
//   click     select
//   dbl       open the session's terminal as a tab in the focused pane
//   middle    close the session tab (never kills the PTY)
//   right     menu: Open in pane · Move to pane · Close tab (the single-
//             pointer alternative to dragging a tab out, WCAG 2.5.7)
//   ←/→       rove (single tab stop)
//   drag      out into a pane's drop zones (`drag-state` source 'dock')

import { useEffect, useRef, useState } from 'react';
import { useDragState } from '@/lib/panes/drag-state';
import { usePaneStore } from '@/lib/panes/pane-store';
import { beginPointerDrag } from '@/lib/panes/pointer-drag';
import type { PaneView } from '@/lib/panes/types';
import { cn } from '@/components/ui/utils';
import { viewLabel } from '@/shell/panes/pane-views';
import { useTerminalStore } from '@/terminal/session-store';
import { useTerminalTitles } from '@/terminal/use-terminal-titles';
import { useCompanionStore } from './companion-store';

type Pulse = 'live' | 'idle' | 'run';

function usePulse(view: PaneView): Pulse {
	return useTerminalStore((s) => {
		if (view.kind !== 'terminal') return 'idle';
		const tab = s.tabs.find((t) => t.id === view.sessionId);
		if (!tab) return 'idle';
		if (tab.status === 'spawning') return 'run';
		return tab.status === 'running' ? 'live' : 'idle';
	});
}

function PulseDot({ pulse }: { pulse: Pulse }) {
	const color =
		pulse === 'live' ? 'var(--live)' : pulse === 'run' ? 'var(--ember)' : 'var(--fg-faint)';
	return (
		<span
			aria-hidden="true"
			className={cn(
				'size-1.5 shrink-0 rounded-full',
				pulse === 'run' && 'motion-safe:animate-pulse'
			)}
			style={{ background: color }}
		/>
	);
}

/** Open a companion tab's view in the focused pane. `move` also removes it
 *  from the Companion (the pointer alternative to drag-out). */
export function openSessionInPane(idx: number, move: boolean): boolean {
	const view = useCompanionStore.getState().tabs[idx];
	if (!view) return false;
	const panes = usePaneStore.getState();
	const placed = panes.placeView(panes.focusedId, { ...view, pinned: undefined }, 'append');
	if (placed && move) useCompanionStore.getState().closeTab(idx);
	return placed;
}

function SessionTab({
	view,
	idx,
	active,
	focusable,
	label,
	onFocusIdx,
	onMenu,
}: {
	view: PaneView;
	idx: number;
	active: boolean;
	focusable: boolean;
	label: string;
	onFocusIdx: (i: number) => void;
	onMenu: (idx: number, x: number, y: number) => void;
}) {
	const pulse = usePulse(view);
	const selectSession = useCompanionStore((s) => s.selectSession);
	const closeTab = useCompanionStore((s) => s.closeTab);
	return (
		<button
			type="button"
			role="tab"
			aria-selected={active}
			tabIndex={focusable ? 0 : -1}
			data-session-tab={idx}
			title={label}
			onFocus={() => onFocusIdx(idx)}
			onClick={() => selectSession(idx)}
			onDoubleClick={() => openSessionInPane(idx, false)}
			onAuxClick={(e) => {
				if (e.button === 1 && !view.pinned) {
					e.preventDefault();
					closeTab(idx);
				}
			}}
			onContextMenu={(e) => {
				e.preventDefault();
				onMenu(idx, e.clientX, e.clientY);
			}}
			onPointerDown={
				view.pinned
					? undefined
					: (e) =>
							beginPointerDrag(e, {
								label,
								onStart: () => useDragState.getState().startDock(idx),
								onEnd: () => useDragState.getState().end(),
							})
			}
			className={cn(
				'flex h-8 min-w-0 flex-1 items-center justify-center gap-1 border-r px-2 text-[11px] last:border-r-0',
				'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
				active
					? 'bg-[var(--bg-surface)] text-[var(--fg)] shadow-[inset_0_2px_0_var(--primary)]'
					: 'text-[var(--fg-muted)] hover:bg-[var(--bg-raised)] hover:text-[var(--fg)]'
			)}
			style={{ borderColor: 'var(--border-soft)' }}
		>
			<PulseDot pulse={pulse} />
			<span className="truncate">{label}</span>
		</button>
	);
}

export function SessionTabs() {
	const tabs = useCompanionStore((s) => s.tabs);
	const activeIdx = useCompanionStore((s) => s.activeIdx);
	const scope = useCompanionStore((s) => s.panelScopeSessionId);
	const closeTab = useCompanionStore((s) => s.closeTab);
	const resolveTerminal = useTerminalTitles();
	const [focusIdx, setFocusIdx] = useState(activeIdx);
	const [menu, setMenu] = useState<{ idx: number; x: number; y: number } | null>(null);
	const listRef = useRef<HTMLDivElement | null>(null);
	const menuRef = useRef<HTMLDivElement | null>(null);

	useEffect(() => {
		if (!menu) return;
		menuRef.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
		const onDown = (e: MouseEvent) => {
			if (!menuRef.current?.contains(e.target as Node)) setMenu(null);
		};
		window.addEventListener('mousedown', onDown);
		return () => window.removeEventListener('mousedown', onDown);
	}, [menu]);

	if (tabs.length === 0) return null;

	function rove(e: React.KeyboardEvent) {
		if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
		e.preventDefault();
		const next = (focusIdx + (e.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length;
		setFocusIdx(next);
		listRef.current?.querySelector<HTMLElement>(`[data-session-tab="${next}"]`)?.focus();
	}

	function closeMenu() {
		const idx = menu?.idx;
		setMenu(null);
		if (idx != null) {
			listRef.current?.querySelector<HTMLElement>(`[data-session-tab="${idx}"]`)?.focus();
		}
	}

	// A tab is "active" only when it is the scoped session — selection is the
	// panel scope, so a stale activeIdx never shows as selected.
	const selectedIdx = tabs.findIndex(
		(t, i) => i === activeIdx && (t.kind !== 'terminal' || t.sessionId === scope)
	);
	const roveIdx = Math.min(focusIdx, tabs.length - 1);

	return (
		<>
			<div
				ref={listRef}
				role="tablist"
				aria-label="Sessions"
				onKeyDown={rove}
				className="flex shrink-0 border-b"
				style={{ borderColor: 'var(--border)', background: 'var(--bg-base)' }}
			>
				{tabs.map((view, idx) => (
					<SessionTab
						key={view.kind === 'terminal' ? `terminal:${view.sessionId}` : JSON.stringify(view)}
						view={view}
						idx={idx}
						active={idx === selectedIdx}
						focusable={idx === roveIdx}
						label={viewLabel(view, resolveTerminal)}
						onFocusIdx={setFocusIdx}
						onMenu={(i, x, y) => setMenu({ idx: i, x, y })}
					/>
				))}
			</div>
			{menu && (
				<div
					ref={menuRef}
					role="menu"
					aria-label="Session tab actions"
					onKeyDown={(e) => {
						const items = Array.from(
							menuRef.current?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? []
						);
						const at = items.indexOf(document.activeElement as HTMLElement);
						if (e.key === 'Escape') {
							e.preventDefault();
							closeMenu();
						} else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
							e.preventDefault();
							const d = e.key === 'ArrowDown' ? 1 : -1;
							items[(at + d + items.length) % items.length]?.focus();
						}
					}}
					className="fixed z-50 w-44 rounded-md border py-1 shadow-lg"
					style={{
						left: menu.x,
						top: menu.y,
						background: 'var(--bg-raised)',
						borderColor: 'var(--border)',
					}}
				>
					{[
						{ label: 'Open in pane', run: () => openSessionInPane(menu.idx, false) },
						{ label: 'Move to pane', run: () => openSessionInPane(menu.idx, true) },
						...(tabs[menu.idx]?.pinned
							? []
							: [{ label: 'Close tab', run: () => closeTab(menu.idx) }]),
					].map((item) => (
						<button
							key={item.label}
							type="button"
							role="menuitem"
							tabIndex={-1}
							onClick={() => {
								item.run();
								closeMenu();
							}}
							className="flex min-h-6 w-full items-center px-3 py-1 text-left text-xs text-[var(--fg)] hover:bg-[var(--bg-sunken)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring"
						>
							{item.label}
						</button>
					))}
				</div>
			)}
		</>
	);
}
