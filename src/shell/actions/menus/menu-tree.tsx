// D-06 Menus tab (WP-59): the ordered tree — renders `MenuRow[]` and owns
// the pointer-drag interaction (drag from the grip, §3339-3373 of
// `designs/actions.html`, ported to React: window-level pointermove/up
// instead of `setPointerCapture`, which jsdom doesn't implement, so this
// stays unit-testable without a real pointer). Reordering itself (the
// write) is the caller's (`index.tsx`) — this only ever reports a finished
// `(from, to)` move, an eye-toggle, or a separator removal upward.
//
// Review round 1 major 6: rows are keyed by `id` alone now (`menu-model.ts`
// — no more `hidden-${id}`), so React keeps the same DOM node for a row
// across a hide/unhide or a re-merge instead of remounting it. That's also
// why a *drag or keyboard* move can restore focus itself: after `onReorder`
// accepts a move, this component remembers the target index and, once the
// next `rows` render lands, focuses whatever is there — for an action row
// that's the same identity (name unchanged); for a separator (no stable id
// across a reorder) it's "whatever ended up at that slot", the best that's
// available. The same effect announces "Moved to position N" through a
// polite live region, matching D-06's own toast (`actions.html:3335`).
//
// Review round 1 major 8 (drag hardening): `pointercancel` gets the same
// cleanup as `pointerup` (minus the reorder itself), so an interrupted drag
// (an OS gesture, a devtools pause) never leaves `body.reordering` stuck or
// listeners attached forever; only the primary button starts a drag;
// `touch-action: none` on the grip (`menus.css`) stops the browser's own
// touch-scroll/refresh gestures from fighting the pointer capture.

import { useEffect, useRef, useState } from 'react';
import type { FocusEvent as ReactFocusEvent, PointerEvent as ReactPointerEvent } from 'react';
import { MenuRowView } from './menu-row';
import type { MenuRow } from './menu-model';

interface DropTarget {
	index: number;
	pos: 'before' | 'after';
}

export interface MenuTreeProps {
	rows: readonly MenuRow[];
	keyById: ReadonlyMap<string, string | null>;
	/** `true` when the move was accepted and (will be) written; `false` when
	 *  it was rejected (e.g. a separator move that would collapse) — this
	 *  component only announces/refocuses on `true`. */
	onReorder: (from: number, to: number) => boolean;
	onToggleHidden: (row: Extract<MenuRow, { kind: 'action' }>) => void;
	onRemoveSeparator: (index: number) => void;
	/** A deep-linked action id (`?action=<id>`, `ActionsSurfaceProps`) to
	 *  focus once it appears in `rows` — focusing it both scrolls it into
	 *  view and gives it the same visible outline a keyboard user gets, with
	 *  no extra highlight CSS needed. */
	highlightId?: string | null;
	/** Double-click on an editable row → the Editor tab for that action. */
	onOpenEditor?: (actionId: string) => void;
	/** Fires when a row gains focus (never on blur) — the caller uses "last
	 *  focused row" as Add separator's insertion point (Blocking 2). */
	onRowFocus?: (index: number) => void;
}

export function MenuTree({
	rows,
	keyById,
	onReorder,
	onToggleHidden,
	onRemoveSeparator,
	highlightId,
	onOpenEditor,
	onRowFocus,
}: MenuTreeProps) {
	const containerRef = useRef<HTMLDivElement | null>(null);
	const [draggingIndex, setDraggingIndex] = useState<number | null>(null);
	const [dropTarget, setDropTarget] = useState<DropTarget | null>(null);
	const [liveMessage, setLiveMessage] = useState('');
	const pendingFocusIndexRef = useRef<number | null>(null);

	useEffect(() => {
		if (!highlightId) return;
		const el = containerRef.current?.querySelector<HTMLElement>(`[data-row-id="${CSS.escape(highlightId)}"]`);
		el?.focus();
	}, [highlightId, rows]);

	useEffect(() => {
		const index = pendingFocusIndexRef.current;
		if (index == null) return;
		pendingFocusIndexRef.current = null;
		const el = containerRef.current?.children[Math.min(index, rows.length - 1)] as HTMLElement | undefined;
		el?.focus();
	}, [rows]);

	function reorderAndAnnounce(from: number, to: number) {
		if (!onReorder(from, to)) return;
		pendingFocusIndexRef.current = to;
		setLiveMessage(`Moved to position ${to + 1}`);
	}

	function onFocusCapture(e: ReactFocusEvent<HTMLDivElement>) {
		const row = (e.target as HTMLElement).closest<HTMLElement>('[data-i]');
		if (!row) return;
		onRowFocus?.(Number(row.dataset.i));
	}

	function onGripPointerDown(e: ReactPointerEvent<HTMLSpanElement>, index: number) {
		if (e.button !== 0) return;
		const container = containerRef.current;
		if (!container) return;
		const rowEls = Array.from(container.children) as HTMLElement[];
		let target = index;
		setDraggingIndex(index);
		document.body.classList.add('reordering');

		function onMove(ev: PointerEvent) {
			// A sentinel (`-1`), not `null` — reassigned inside `forEach`'s
			// closure, and a numeric comparison narrows cleanly afterwards
			// without leaning on cross-closure `null` narrowing.
			let best = -1;
			let bestPos = 'after' as 'before' | 'after';
			rowEls.forEach((el, i) => {
				if (i === index) return;
				const box = el.getBoundingClientRect();
				if (ev.clientY >= box.top && ev.clientY <= box.bottom) {
					best = i;
					bestPos = ev.clientY < box.top + box.height / 2 ? 'before' : 'after';
				}
			});
			if (best === -1) return;
			setDropTarget({ index: best, pos: bestPos });
			target = bestPos === 'before' ? (best > index ? best - 1 : best) : best < index ? best + 1 : best;
		}
		function cleanup() {
			window.removeEventListener('pointermove', onMove);
			window.removeEventListener('pointerup', onUp);
			window.removeEventListener('pointercancel', onCancel);
			document.body.classList.remove('reordering');
			setDraggingIndex(null);
			setDropTarget(null);
		}
		function onUp() {
			cleanup();
			if (target !== index) reorderAndAnnounce(index, target);
		}
		function onCancel() {
			cleanup();
		}
		window.addEventListener('pointermove', onMove);
		window.addEventListener('pointerup', onUp);
		window.addEventListener('pointercancel', onCancel);
	}

	return (
		<>
			<div className="tree2" ref={containerRef} role="list" aria-label="Menu items" onFocus={onFocusCapture}>
				{rows.map((row, index) => (
					<MenuRowView
						key={row.key}
						row={row}
						index={index}
						isFirst={index === 0}
						isLast={index === rows.length - 1}
						isDragging={draggingIndex === index}
						dropIndicator={dropTarget && dropTarget.index === index ? dropTarget.pos : null}
						keyCombo={row.kind === 'action' ? (keyById.get(row.id) ?? null) : null}
						onGripPointerDown={onGripPointerDown}
						onMoveUp={() => reorderAndAnnounce(index, index - 1)}
						onMoveDown={() => reorderAndAnnounce(index, index + 1)}
						onToggleHidden={() => row.kind === 'action' && onToggleHidden(row)}
						onRemoveSeparator={() => onRemoveSeparator(index)}
						onOpenEditor={row.kind === 'action' && onOpenEditor ? () => onOpenEditor(row.id) : undefined}
					/>
				))}
			</div>
			{/* Outside `.tree2` on purpose: `onGripPointerDown`'s hit-testing reads
			 *  `containerRef.current.children` positionally against `rows`, and a
			 *  sibling here (rather than a trailing child) keeps that indexing
			 *  exact. */}
			<div aria-live="polite" role="status" className="sr-only">
				{liveMessage}
			</div>
		</>
	);
}
