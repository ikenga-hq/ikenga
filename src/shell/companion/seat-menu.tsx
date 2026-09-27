// WP-67 — the seat `⋯` / right-click menu and the unseated-session menu
// (D-09 `seats-companion.html` `seatMenu` / `sessMenu`, G-SEATS §4.4, §5.5).
//
// Item order is the locked file's: Open in pane · Make dispatch target ·
// Open scratchpad · Pop out · All seats │ Rename… · Copy address · Copy as
// iyke │ End session · Remove seat…. *Take over* (Round 45, §5.5) is added
// only while another client holds the seat, so the resting menu is D-09's.
//
// **Pop out** (G-97): this file holds the one call site. Today it calls the
// pop-out that ships (`spawnWindow`, always a new window); WP-69 changes
// only what `popOutTerminal` does (join Window 2, DEC-69d) under its scoped
// exception, and nothing else in the Companion.

import { useEffect, useRef } from 'react';
import { cn } from '@/components/ui/utils';
import { spawnWindow } from '@/lib/tauri-cmd';
import { markSurfaceDetached, syncDetachedSurfaces } from '@/lib/window/detached-surfaces';
import { useTerminalStore } from '@/terminal/session-store';
import { showSeatNotice } from './seat-notice';

// ─── Pop out (the single call site, G-97) ───────────────────────────────────

/**
 * Pop a terminal out to a second window. The seat row never changes: the
 * address is not the mount (D-09 rule 2, §4.4). `name` is the seat's name,
 * or the session's label for an unseated one.
 */
export function popOutTerminal(terminalId: string, name: string): void {
	const tab = useTerminalStore.getState().tabs.find((t) => t.id === terminalId);
	const ptyId = tab?.ptyId;
	if (!ptyId || tab?.status !== 'running') {
		showSeatNotice(`${name}’s terminal isn’t running — nothing to pop out`, { variant: 'error' });
		return;
	}
	const surfaceId = `terminal:${ptyId}`;
	const label = `detached-terminal-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
	// Optimistic, exactly like the pane's own pop-out: the pane swaps to its
	// placeholder at once instead of briefly duplicating the live terminal.
	markSurfaceDetached(surfaceId, label);
	spawnWindow({
		label,
		kind: 'single-surface',
		surface_set: [surfaceId],
		project_id: null,
		layout_key: label,
	})
		.then(() => showSeatNotice(`${name} moved to Window 2 — its address is unchanged`))
		.catch((err: unknown) => {
			void syncDetachedSurfaces();
			showSeatNotice(`Couldn’t pop out ${name}: ${err instanceof Error ? err.message : String(err)}`, {
				variant: 'error',
			});
		});
}

// ─── The menu ───────────────────────────────────────────────────────────────

export type SeatMenuItem =
	| { sep: true }
	| {
			sep?: false;
			label: string;
			/** Trailing muted text (`seat:royalti-co/lead`, `F2`). */
			sub?: string;
			disabled?: boolean;
			/** Why it is disabled, or what it does. */
			title?: string;
			danger?: boolean;
			run: () => void;
	  };

export function SeatMenu({
	label,
	x,
	y,
	items,
	onClose,
}: {
	/** Accessible name, e.g. "Seat actions for @lead". */
	label: string;
	x: number;
	y: number;
	items: SeatMenuItem[];
	/** Close; `restoreFocus` is false when the click landed elsewhere. */
	onClose: (restoreFocus: boolean) => void;
}) {
	const ref = useRef<HTMLDivElement | null>(null);
	// Read through a ref: the rail re-renders on every roster refetch, and a
	// fresh `onClose` must not re-run the mount effect (it would steal focus
	// back to the first item).
	const closeRef = useRef(onClose);
	closeRef.current = onClose;

	useEffect(() => {
		ref.current?.querySelector<HTMLElement>('[role="menuitem"]:not([disabled])')?.focus();
		const onDown = (e: MouseEvent) => {
			if (!ref.current?.contains(e.target as Node)) closeRef.current(false);
		};
		window.addEventListener('mousedown', onDown);
		return () => window.removeEventListener('mousedown', onDown);
	}, []);

	function onKeyDown(e: React.KeyboardEvent) {
		const enabled = Array.from(
			ref.current?.querySelectorAll<HTMLElement>('[role="menuitem"]:not([disabled])') ?? []
		);
		const at = enabled.indexOf(document.activeElement as HTMLElement);
		if (e.key === 'Escape') {
			e.preventDefault();
			e.stopPropagation();
			onClose(true);
		} else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
			e.preventDefault();
			const d = e.key === 'ArrowDown' ? 1 : -1;
			enabled[(at + d + enabled.length) % enabled.length]?.focus();
		} else if (e.key === 'Home') {
			e.preventDefault();
			enabled[0]?.focus();
		} else if (e.key === 'End') {
			e.preventDefault();
			enabled.at(-1)?.focus();
		} else if (e.key === 'Tab') {
			e.preventDefault();
			onClose(true);
		}
	}

	// Keep the menu inside the viewport (it opens at the pointer or the ⋯).
	const vw = typeof window !== 'undefined' ? window.innerWidth : 1440;
	const vh = typeof window !== 'undefined' ? window.innerHeight : 900;
	const left = Math.max(4, Math.min(x, vw - 244));
	const top = Math.max(4, Math.min(y, vh - (items.length * 26 + 16)));

	return (
		<div
			ref={ref}
			role="menu"
			aria-label={label}
			onKeyDown={onKeyDown}
			className="fixed z-50 w-60 rounded-md border py-1 shadow-lg"
			style={{ left, top, background: 'var(--bg-raised)', borderColor: 'var(--border)' }}
		>
			{items.map((item, i) =>
				item.sep ? (
					<div
						// biome-ignore lint/suspicious/noArrayIndexKey: separators have no identity
						key={`sep-${i}`}
						role="separator"
						className="my-1 h-px"
						style={{ background: 'var(--border-soft)' }}
					/>
				) : (
					<button
						key={item.label}
						type="button"
						role="menuitem"
						tabIndex={-1}
						disabled={item.disabled}
						title={item.title || undefined}
						onClick={() => {
							onClose(true);
							item.run();
						}}
						className={cn(
							'flex min-h-6 w-full items-center gap-2 px-3 py-1 text-left text-xs',
							'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring',
							'disabled:cursor-not-allowed disabled:opacity-60 enabled:hover:bg-[var(--bg-sunken)]',
							item.danger ? 'text-[var(--color-text-danger)]' : 'text-[var(--fg)]'
						)}
					>
						<span className="truncate">{item.label}</span>
						{item.sub && (
							<span
								className="ml-auto truncate pl-2 font-mono text-[11px]"
								style={{ color: 'var(--fg-muted)' }}
							>
								{item.sub}
							</span>
						)}
					</button>
				)
			)}
		</div>
	);
}
