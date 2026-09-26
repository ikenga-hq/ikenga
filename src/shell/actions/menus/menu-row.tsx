// D-06 Menus tab (WP-59): one tree row — a drag handle, the item (or a
// separator), and the trailing controls. Presentational only: reorder, hide
// and remove all live in `menu-tree.tsx`, which owns the row list and the
// write.
//
// 6A.10 (Round 6, WCAG 2.5.7): a keyboard-only ⌥↑/⌥↓ alternative to drag
// isn't a *pointer* alternative, so this row also renders Move up/Move down
// buttons beside the grip — `menus.css` reveals them on hover/focus-within
// (reserved-slot opacity, the 6A.1 pattern) so the idle tree still matches
// D-06's static `menus` screenshot pixel-for-pixel.

import type { KeyboardEvent as ReactKeyboardEvent, PointerEvent as ReactPointerEvent } from 'react';
import { ChevronDown, ChevronUp, Eye, EyeOff, GripVertical, Shield, X } from 'lucide-react';
import { ActionIcon } from '../shared/action-icon';
import { Kbd } from '../shared/kbd';
import type { MenuRow } from './menu-model';

const SOURCE_LABEL: Record<string, string> = {
	builtin: 'Built-in',
	package: 'Package',
	personal: 'Yours',
	project: 'Yours',
};

export interface MenuRowViewProps {
	row: MenuRow;
	index: number;
	isFirst: boolean;
	isLast: boolean;
	isDragging: boolean;
	dropIndicator: 'before' | 'after' | null;
	keyCombo: string | null;
	onGripPointerDown: (e: ReactPointerEvent<HTMLSpanElement>, index: number) => void;
	onMoveUp: () => void;
	onMoveDown: () => void;
	onToggleHidden: () => void;
	onRemoveSeparator: () => void;
	/** Double-click opens this row's action in the Editor tab — only wired
	 *  for editable (personal/project) actions; a built-in or package action
	 *  has nothing to open there (§9.1). */
	onOpenEditor?: () => void;
}

export function MenuRowView({
	row,
	index,
	isFirst,
	isLast,
	isDragging,
	dropIndicator,
	keyCombo,
	onGripPointerDown,
	onMoveUp,
	onMoveDown,
	onToggleHidden,
	onRemoveSeparator,
	onOpenEditor,
}: MenuRowViewProps) {
	const locked = row.kind === 'action' && row.locked;

	function onKeyDown(e: ReactKeyboardEvent<HTMLDivElement>) {
		if (!e.altKey) return;
		if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
		e.preventDefault();
		if (e.key === 'ArrowUp') onMoveUp();
		else onMoveDown();
	}

	const moveButtons = (
		<span className="movebtns">
			<button type="button" className="movebtn" disabled={isFirst} aria-label="Move up" onClick={onMoveUp}>
				<ChevronUp className="h-3 w-3" aria-hidden="true" />
			</button>
			<button type="button" className="movebtn" disabled={isLast} aria-label="Move down" onClick={onMoveDown}>
				<ChevronDown className="h-3 w-3" aria-hidden="true" />
			</button>
		</span>
	);

	const classes = [
		'itemrow',
		row.kind === 'separator' ? 'sep2' : '',
		row.kind === 'action' && row.hidden ? 'off' : '',
		locked ? 'locked' : '',
		isDragging ? 'dragging' : '',
		dropIndicator === 'before' ? 'dropbefore' : '',
		dropIndicator === 'after' ? 'dropafter' : '',
	]
		.filter(Boolean)
		.join(' ');

	if (row.kind === 'separator') {
		return (
			<div className={classes} role="listitem" tabIndex={0} data-i={index} onKeyDown={onKeyDown}>
				<span className="grip" aria-hidden="true" onPointerDown={(e) => onGripPointerDown(e, index)}>
					<GripVertical className="h-3.5 w-3.5" aria-hidden="true" />
				</span>
				{moveButtons}
				<span className="line" />
				<span style={{ fontFamily: 'var(--font-mono)', fontSize: 10 }}>separator</span>
				<span className="line" />
				<span className="rt">
					<button type="button" className="eye" aria-label="Remove separator" onClick={onRemoveSeparator}>
						<X className="h-3.5 w-3.5" aria-hidden="true" />
					</button>
				</span>
			</div>
		);
	}

	const { action } = row;
	const sourceLabel = SOURCE_LABEL[action.source] ?? action.source;
	const editable = action.editable && onOpenEditor;

	return (
		<div
			className={classes}
			role="listitem"
			tabIndex={0}
			data-i={index}
			data-row-id={action.id}
			onKeyDown={onKeyDown}
			onDoubleClick={editable ? onOpenEditor : undefined}
			title={editable ? `Double-click to edit "${action.name}"` : undefined}
		>
			<span className="grip" aria-hidden="true" onPointerDown={(e) => onGripPointerDown(e, index)}>
				<GripVertical className="h-3.5 w-3.5" aria-hidden="true" />
			</span>
			{moveButtons}
			<ActionIcon icon={action.icon} className="h-3.5 w-3.5 shrink-0" />
			<span className="lbl2">{action.name}</span>
			{keyCombo && <Kbd combo={keyCombo} />}
			<span className="rt">
				<span className={`kind k-${action.source}`}>
					<span className="src-dot" aria-hidden="true" />
					{sourceLabel}
				</span>
				{locked && (
					<span className="lock" title="Locked — may be reordered, never hidden">
						<Shield className="h-3 w-3" aria-hidden="true" />
					</span>
				)}
				{row.hiddenElsewhere && !row.hiddenHere && (
					<span className="hiddenelsewhere" title={`Hidden at the ${row.hiddenElsewhere} scope — unhide it there`}>
						Hidden ({row.hiddenElsewhere})
					</span>
				)}
				<button
					type="button"
					className="eye"
					aria-disabled={locked || undefined}
					title={
						locked
							? 'Locked — it may be reordered, never hidden. Its key is unaffected either way.'
							: row.hiddenElsewhere && !row.hiddenHere
								? `Hidden at the ${row.hiddenElsewhere} scope — unhide it there`
								: row.hidden
									? `Show ${action.name} in this menu`
									: `Hide ${action.name} from this menu — its key still fires (DEC-58)`
					}
					aria-label={`${row.hidden ? 'Show' : 'Hide'} ${action.name}`}
					aria-pressed={row.hidden}
					onClick={onToggleHidden}
				>
					{row.hidden ? <EyeOff className="h-3.5 w-3.5" aria-hidden="true" /> : <Eye className="h-3.5 w-3.5" aria-hidden="true" />}
				</button>
			</span>
		</div>
	);
}
