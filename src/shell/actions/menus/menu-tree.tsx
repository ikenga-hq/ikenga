// D-06 Menus tab (WP-59): the ordered tree — renders `MenuRow[]` and owns
// the pointer-drag interaction (drag from the grip, §3339-3373 of
// `designs/actions.html`, ported to React: window-level pointermove/up
// instead of `setPointerCapture`, which jsdom doesn't implement, so this
// stays unit-testable without a real pointer). Reordering itself (the
// write) is the caller's (`index.tsx`) — this only ever reports a finished
// `(from, to)` move, an eye-toggle, or a separator removal upward.

import { useEffect, useRef, useState } from 'react';
import type { PointerEvent as ReactPointerEvent } from 'react';
import { MenuRowView } from './menu-row';
import type { MenuRow } from './menu-model';

interface DropTarget {
	index: number;
	pos: 'before' | 'after';
}

export interface MenuTreeProps {
	rows: readonly MenuRow[];
	keyById: ReadonlyMap<string, string | null>;
	onReorder: (from: number, to: number) => void;
	onToggleHidden: (row: Extract<MenuRow, { kind: 'action' }>) => void;
	onRemoveSeparator: (index: number) => void;
	/** A deep-linked action id (`?action=<id>`, `ActionsSurfaceProps`) to
	 *  focus once it appears in `rows` — focusing it both scrolls it into
	 *  view and gives it the same visible outline a keyboard user gets, with
	 *  no extra highlight CSS needed. */
	highlightId?: string | null;
	/** Double-click on an editable row → the Editor tab for that action. */
	onOpenEditor?: (actionId: string) => void;
}

export function MenuTree({ rows, keyById, onReorder, onToggleHidden, onRemoveSeparator, highlightId, onOpenEditor }: MenuTreeProps) {
	const containerRef = useRef<HTMLDivElement | null>(null);
	const [draggingIndex, setDraggingIndex] = useState<number | null>(null);
	const [dropTarget, setDropTarget] = useState<DropTarget | null>(null);

	useEffect(() => {
		if (!highlightId) return;
		const el = containerRef.current?.querySelector<HTMLElement>(`[data-row-id="${CSS.escape(highlightId)}"]`);
		el?.focus();
	}, [highlightId, rows]);

	function onGripPointerDown(_e: ReactPointerEvent<HTMLSpanElement>, index: number) {
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
			let bestPos: 'before' | 'after' = 'after';
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
		function onUp() {
			window.removeEventListener('pointermove', onMove);
			window.removeEventListener('pointerup', onUp);
			document.body.classList.remove('reordering');
			setDraggingIndex(null);
			setDropTarget(null);
			if (target !== index) onReorder(index, target);
		}
		window.addEventListener('pointermove', onMove);
		window.addEventListener('pointerup', onUp);
	}

	return (
		<div className="tree2" ref={containerRef} role="list" aria-label="Menu items">
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
					onMoveUp={() => onReorder(index, index - 1)}
					onMoveDown={() => onReorder(index, index + 1)}
					onToggleHidden={() => row.kind === 'action' && onToggleHidden(row)}
					onRemoveSeparator={() => onRemoveSeparator(index)}
					onOpenEditor={row.kind === 'action' && onOpenEditor ? () => onOpenEditor(row.id) : undefined}
				/>
			))}
		</div>
	);
}
