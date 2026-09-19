// Drop targets for tab moves, overlaid on each pane's body area.
//
// Always mounted; while a tab drag is active (`useDragState`, published by a
// pane-strip or dock tab through `beginPointerDrag`) the overlay switches on
// pointer-events so the drag controller's hit-test finds it. It reports a
// pointer-relative zone (4 edges or center) and either dispatches a `moveTab`
// action (pane-source) or transfers the view from the dock into the pane
// (dock-source). Pointer events rather than HTML5 DnD: see
// `lib/panes/pointer-drag.ts`.
//
// Edge inset is 25% of the pane's width/height. Outside that ring the
// zone is `'center'` (move-as-tab). Inside it, `'left'` / `'right'`
// split the pane horizontally (panes side-by-side); `'top'` / `'bottom'`
// split vertically. Edge zones are blocked when the tree is at the
// 6-leaf cap; center still works.

import { useState } from 'react';

import { useDragState } from '@/lib/panes/drag-state';
import { useDropTarget } from '@/lib/panes/pointer-drag';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useCompanionStore } from '@/shell/companion/companion-store';
import { MAX_LEAVES, type PaneId } from '@/lib/panes/types';
import { cn } from '@/components/ui/utils';
import { FloatingToastChip } from '@/components/ui/floating-toast-chip';

const EDGE_INSET = 0.25;

type Zone = 'center' | 'top' | 'right' | 'bottom' | 'left';

function detectZone(rect: DOMRect, x: number, y: number): Zone {
	const relX = (x - rect.left) / rect.width;
	const relY = (y - rect.top) / rect.height;
	if (relX < EDGE_INSET) return 'left';
	if (relX > 1 - EDGE_INSET) return 'right';
	if (relY < EDGE_INSET) return 'top';
	if (relY > 1 - EDGE_INSET) return 'bottom';
	return 'center';
}

export function PaneDropZones({ paneId }: { paneId: PaneId }) {
	const drag = useDragState();
	const moveTab = usePaneStore((s) => s.moveTab);
	const placeView = usePaneStore((s) => s.placeView);
	const canSplit = usePaneStore((s) => s.canSplit());
	const [hoverZone, setHoverZone] = useState<Zone | null>(null);
	// §4.3: the drag-to-edge split entry point is blocked at the 6-leaf cap
	// (edge zones stay lit but disabled, above); a dropped edge attempt there
	// also surfaces the shared floating toast so the reason isn't silent.
	const [showCapToast, setShowCapToast] = useState(false);

	// No-op self-drag of a pane's only tab (pane-source only — dock id can
	// never collide with a pane id).
	const sameAsSrc = drag.source === 'pane' && drag.srcLeafId === paneId;

	const dropTarget = useDropTarget({
		// §4.3: at the split cap, edge zones stay lit but in a disabled
		// treatment (so the cap is legible mid-drag) instead of not
		// rendering at all — `accepts` stays true for every zone; the cap is
		// enforced in `onDrop` below. `center` (move-as-tab) is never capped.
		accepts: () => useDragState.getState().active,
		onOver: (x, y, el) => {
			const zone = detectZone(el.getBoundingClientRect(), x, y);
			setHoverZone((prev) => (prev === zone ? prev : zone));
		},
		onLeave: () => setHoverZone(null),
		onDrop: (x, y, el) => {
			setHoverZone(null);
			const d = useDragState.getState();
			if (!d.active || d.srcTabIdx == null) {
				d.end();
				return;
			}
			const zone = detectZone(el.getBoundingClientRect(), x, y);
			// Edge zones are blocked at the 6-leaf cap; `center` still works.
			if (zone !== 'center' && !canSplit) {
				setShowCapToast(true);
				d.end();
				return;
			}
			const mode = zone === 'center' ? 'append' : zone;

			if (d.source === 'pane') {
				if (d.srcLeafId != null) moveTab(d.srcLeafId, d.srcTabIdx, paneId, mode);
			} else if (d.source === 'dock') {
				// Dock → pane: pull the view out of the dock store and place it via
				// the pane store. Only close from the dock if the placement succeeds.
				const view = useCompanionStore.getState().tabs[d.srcTabIdx];
				if (view && placeView(paneId, view, mode)) {
					useCompanionStore.getState().closeTab(d.srcTabIdx);
				}
			}
			d.end();
		},
	});

	return (
		<div
			{...dropTarget}
			className={cn(
				'absolute inset-0 z-20',
				// Invisible to the hit-test (and to clicks) unless a drag is in flight.
				drag.active ? 'pointer-events-auto' : 'pointer-events-none'
			)}
			data-testid={`drop-zone-${paneId}`}
		>
			<ZoneIndicator
				zone={hoverZone}
				dimmed={sameAsSrc && hoverZone === 'center'}
				disabled={hoverZone !== null && hoverZone !== 'center' && !canSplit}
			/>
			{showCapToast && (
				<FloatingToastChip
					variant="notice"
					anchor="pane-corner"
					label={`Pane limit reached (${MAX_LEAVES}).`}
					onDismiss={() => setShowCapToast(false)}
					ttlMs={2500}
				/>
			)}
		</div>
	);
}

interface ZoneIndicatorProps {
	zone: Zone | null;
	dimmed: boolean;
	/** §4.3: an edge zone at the 6-leaf cap — stays visible, disabled
	 *  treatment, so the cap is legible mid-drag instead of the zone just
	 *  not lighting up. */
	disabled: boolean;
}

function ZoneIndicator({ zone, dimmed, disabled }: ZoneIndicatorProps) {
	if (!zone) return null;
	const base = cn(
		'absolute transition-opacity',
		disabled
			? 'border-2 border-muted-foreground/40 bg-muted-foreground/10 cursor-not-allowed'
			: 'border-2 border-primary/70 bg-primary/15'
	);
	const positional = (() => {
		switch (zone) {
			case 'center':
				return 'inset-3 rounded-md';
			case 'left':
				return 'left-0 top-0 bottom-0 w-1/4 rounded-r-md';
			case 'right':
				return 'right-0 top-0 bottom-0 w-1/4 rounded-l-md';
			case 'top':
				return 'top-0 left-0 right-0 h-1/4 rounded-b-md';
			case 'bottom':
				return 'bottom-0 left-0 right-0 h-1/4 rounded-t-md';
		}
	})();
	return <div className={cn(base, positional, dimmed && 'opacity-30')} />;
}
