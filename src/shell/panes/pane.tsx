import { useCallback } from 'react';
import type { LeafNode } from '@/lib/panes/types';
import { hasAddressBar } from '@/lib/panes/pane-address';
import { usePaneStore } from '@/lib/panes/pane-store';
import { PaneAddressBar } from './pane-address-bar';
import { PaneIykeOverlay } from './pane-iyke-overlay';
import { PaneTabStrip } from './pane-tab-strip';
import { PaneTools } from './pane-toolbar';
import { PaneBody } from './pane-views';
import { tabUid } from './view-key';
import { PaneDropZones } from './drop-zones';
import { cn } from '@/components/ui/utils';

interface PaneProps {
	leaf: LeafNode;
}

export function Pane({ leaf }: PaneProps) {
	// Subscribe to a *boolean* derived from focusedId, not focusedId itself.
	// Otherwise every focus change re-renders every pane in the tree; with
	// the boolean selector each pane only re-renders when its own focused
	// state flips (twice per focus change cluster: old + new).
	const isFocused = usePaneStore((s) => s.focusedId === leaf.id);
	const focusPane = usePaneStore((s) => s.focusPane);
	const refreshTick = usePaneStore((s) => s.refreshTicks[leaf.id] ?? 0);
	const activeTab = leaf.tabs[leaf.activeTabIdx];

	// §6A.3: a pane with exactly one tab draws a single merged row (address +
	// tools); the tab strip returns as soon as a second tab exists.
	const isSingleTab = leaf.tabs.length === 1;

	// Capture-phase focus: any click anywhere in the pane focuses it. Stops
	// short of stealing keyboard focus from the user's actual click target.
	const handleFocusCapture = useCallback(() => {
		if (!isFocused) focusPane(leaf.id);
	}, [isFocused, focusPane, leaf.id]);

	return (
		<div
			onMouseDownCapture={handleFocusCapture}
			onFocusCapture={handleFocusCapture}
			className={cn(
				// `group/pane` names the hover/focus-within scope the pane-tools
				// reveal (§6A.1) reacts to — see pane-toolbar.tsx.
				'leaf group/pane flex h-full w-full flex-col overflow-hidden',
				'bg-background',
				'ring-inset transition-shadow',
				isFocused ? 'ring-1' : 'ring-0'
			)}
			style={
				isFocused
					? {
							// §a11y line (WCAG 1.4.11, non-text contrast ≥ 3:1): 40%
							// measured 2.05:1 against the dark-theme pane background
							// (`e2e/panes.spec.ts`'s "a11y line" check) — bumped to 65%
							// (≈3.6:1, margin over the 3.25:1 the 60% floor gives).
							['--tw-ring-color' as string]:
								'color-mix(in srgb, var(--tint-fg-active, var(--primary)) 65%, transparent)',
						}
					: undefined
			}
			data-pane-id={leaf.id}
			data-focused={isFocused ? 'true' : 'false'}
		>
			{isSingleTab && activeTab && hasAddressBar(activeTab) ? (
				// §6A.3 merged row: the address (path or route), tab title as its
				// `aria-label`, then the tools — one row, not the tab-strip row
				// plus a second address-bar row underneath.
				<PaneAddressBar paneId={leaf.id} view={activeTab} leaf={leaf} mergedTools />
			) : (
				// Address-less kinds (terminal, scratchpad) already draw as one
				// row today — the tab strip and tools have always shared this
				// flex row, they just used to be two separate always-visible
				// controls; PaneTools is now the reveal-gated pair.
				<div className="flex shrink-0 items-stretch border-b border-border">
					<div className="flex-1 min-w-0">
						<PaneTabStrip leaf={leaf} isFocused={isFocused} />
					</div>
					<div className="flex shrink-0 items-center px-1.5">
						<PaneTools paneId={leaf.id} />
					</div>
				</div>
			)}
			{!isSingleTab && activeTab && hasAddressBar(activeTab) && (
				<PaneAddressBar paneId={leaf.id} view={activeTab} />
			)}
			<div className="relative flex-1 min-h-0 overflow-hidden">
				{activeTab && (
					<PaneBody
						// Keyed by the tab's own stable identity (not activeTabIdx, a
						// POSITION) so dragging the active tab or closing a
						// lower-indexed sibling doesn't shift this index and force a
						// spurious remount of a view the user only moved. refreshTick
						// stays in the key — an explicit refresh must still remount.
						key={`${leaf.id}:${tabUid(activeTab)}:${refreshTick}`}
						paneId={leaf.id}
						view={activeTab}
					/>
				)}
				<PaneDropZones paneId={leaf.id} />
				<PaneIykeOverlay paneId={leaf.id} />
			</div>
		</div>
	);
}
