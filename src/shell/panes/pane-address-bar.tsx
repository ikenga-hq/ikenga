// Browser-style URL bar shown above pane content for path-bearing views
// (route, artifact). Terminal panes render no address bar — they
// don't have a natural address.
//
// Layout:  [← back] [→ forward] [↻ refresh] [editable URL input]
//
// `replace(view)` swaps the leaf's active view in place and pushes a
// history entry. `bumpKey()` re-mounts the leaf so refresh resets viewer
// state. Invalid input rings the input red briefly without navigating.

import { ArrowLeft, ArrowRight, Pin as PinGlyph, Plus, RefreshCw } from 'lucide-react';
import { useCallback, useEffect, useRef, useState } from 'react';
import { cn } from '@/components/ui/utils';
import { IconButton } from '@/components/ui/icon-button';
import { Input } from '@/components/ui/input';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import { formatPaneAddressForDisplay, parsePaneAddress } from '@/lib/panes/pane-address';
import { resolveArtifactAddress } from '@/lib/panes/pane-address-resolver';
import type { LeafNode, PaneId, PaneView } from '@/lib/panes/types';
import { usePaneHistory } from '@/lib/panes/use-pane-history';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useDragState } from '@/lib/panes/drag-state';
import { beginPointerDrag } from '@/lib/panes/pointer-drag';
import { writeClipboardText } from '@/lib/transport';
import { usePathToManifestId, usePinsStore } from '@/lib/shell/pins-store';
import { PinArtifactDialog } from './pin-artifact-dialog';
import { PaneTools } from './pane-toolbar';
import { viewLabel } from './pane-views';
import { NewTabMenu, useAnchorRect } from './new-tab-menu';

interface PaneAddressBarProps {
	paneId: PaneId;
	view: PaneView;
	/** §6A.3 merged row only: the leaf this address bar stands in for — its
	 *  one tab's title becomes the row's `aria-label` (a browser-style URL
	 *  bar has no room to also show the tab title as visible text), and its
	 *  "+ New tab" affordance rides along in the same row since the tab
	 *  strip that normally carries it doesn't render for a single tab. */
	leaf?: LeafNode;
	/** §6A.3 merged row only: appends the reveal-gated `⟳ + ⋯` tools cluster
	 *  to this same row instead of a separate tab-strip row rendering them. */
	mergedTools?: boolean;
}

const INVALID_FLASH_MS = 600;

export function PaneAddressBar({ paneId, view, leaf, mergedTools }: PaneAddressBarProps) {
	const { canGoBack, canGoForward, back, forward, replace, bumpKey } = usePaneHistory(paneId, view);

	// Display rule (Phase 3): pinned artifacts show their canonical
	// `ikenga://artifact/<id>` URI; everything else shows the raw path. The
	// URI survives renames + makes it obvious what the canonical address is
	// for sharing or pasting elsewhere.
	const pathToManifestId = usePathToManifestId();
	const address = formatPaneAddressForDisplay(view, pathToManifestId) ?? '';
	const [draft, setDraft] = useState(address);
	const [invalid, setInvalid] = useState(false);
	const inputRef = useRef<HTMLInputElement | null>(null);

	// External navigations (sidebar click, command palette, back/forward)
	// should sync into the input. We only adopt the upstream value when the
	// user isn't actively editing — checked by focus.
	useEffect(() => {
		if (document.activeElement !== inputRef.current) {
			setDraft(address);
		}
	}, [address]);

	const flashInvalid = useCallback(() => {
		setInvalid(true);
		window.setTimeout(() => setInvalid(false), INVALID_FLASH_MS);
	}, []);

	const submit = useCallback(async () => {
		const parsed = parsePaneAddress(draft);
		if (!parsed) {
			flashInvalid();
			return;
		}
		// `ikenga://artifact/<id>` keeps the literal URI in `path` after
		// parsing — resolve to the on-disk path before navigating. Other
		// address shapes pass through unchanged.
		const { view: resolved, resolved: needed } = await resolveArtifactAddress(parsed);
		if (!resolved) {
			// Resolver only returns null when it actually tried (an `ikenga://`
			// id with no matching pin). Treat that the same as a parse fail.
			if (needed) flashInvalid();
			return;
		}
		// No-op if it matches the current address exactly (avoid cluttering
		// history when the user just hits Enter on what's already loaded).
		// Compare via the display formatter so typing the URI for the
		// currently-shown URI counts as a refresh, not a navigation.
		if (formatPaneAddressForDisplay(resolved, pathToManifestId) === address) {
			bumpKey();
			return;
		}
		replace(resolved);
	}, [draft, address, pathToManifestId, replace, bumpKey, flashInvalid]);

	// Pin button is artifact-only and lights up amber when this exact path
	// is already pinned (so the user knows clicking again would be a dup).
	// Future: clicking when already pinned could open an edit dialog;
	// today it's hidden entirely to avoid suggesting a dup-create.
	const pinForCurrentPath = usePinsStore((s) =>
		view.kind === 'artifact' ? (s.pins.find((p) => p.target === view.path) ?? null) : null
	);
	const [pinDialogOpen, setPinDialogOpen] = useState(false);

	// §6A.3: in the merged row the tab title becomes this row's accessible
	// label (a browser-style address bar has no visible room for it), and
	// the address text itself is what a `.leaf.micro` container query hides
	// first, leaving the icon-less row to just its aria-label + tools.
	const rowLabel = leaf ? viewLabel(leaf.tabs[0]) : undefined;

	const focusPane = usePaneStore((s) => s.focusPane);
	const closeTab = usePaneStore((s) => s.closeTab);
	const toggleTabPinned = usePaneStore((s) => s.toggleTabPinned);
	const [newTabOpen, setNewTabOpen] = useState(false);
	const addBtnRef = useRef<HTMLButtonElement | null>(null);
	const newTabAnchor = useAnchorRect(newTabOpen, addBtnRef);

	// §6A.3 deviation fix: a merged single-tab row has no tab strip, so its
	// one tab has no other way to reach the tab-level actions (Close, Pin
	// tab, Copy path) or to become a drag source for a cross-pane move. The
	// address text itself stands in for the tab here — same context menu
	// items the 2+-tab strip offers on a `Tab`, minus the reorder/"move to
	// new pane" items that only make sense with a sibling tab to move past.
	const soleTab = leaf?.tabs[0];
	const isSoleTabPinned = Boolean(soleTab?.pinned);
	const soleTabPath = soleTab?.kind === 'artifact' || soleTab?.kind === 'route' ? soleTab.path : undefined;
	// `address` (G-ACTIONS §1.3), resolved only while the menu is open. The
	// default contents carry no applicability for "Pin to sidebar…"; it has a
	// handler (and so renders) only on an artifact tab, as shipped.
	const addressMenuHandlers: Record<string, () => void> = {
		...(soleTab?.kind === 'artifact' ? { 'pin-sidebar': () => setPinDialogOpen(true) } : {}),
		'tab.toggle-pin': () => {
			if (leaf) toggleTabPinned(leaf.id, 0);
		},
		...(soleTabPath !== undefined
			? { 'copy-path': () => void writeClipboardText(soleTabPath).catch(() => {}) }
			: {}),
		'tab.close': () => {
			if (leaf) closeTab(leaf.id, 0);
		},
	};

	return (
		<div
			className="flex shrink-0 items-center gap-0.5 border-b border-border bg-background px-1.5 py-1"
			role="toolbar"
			aria-label={rowLabel}
		>
			{/* §6A.1 / DoD P3: in the merged row, Back/Forward fold into the `⋯`
			    menu (PaneTools' `history` prop below) instead of two more
			    always-visible icons — that's what keeps a multi-pane resting
			    layout under the reference control count. The non-merged
			    (2+ tab) address bar keeps its own Back/Forward, unchanged. */}
			{!mergedTools && (
				<>
					<IconButton onClick={() => back()} disabled={!canGoBack} title="Back" aria-label="Back">
						<ArrowLeft className="h-3.5 w-3.5" />
					</IconButton>
					<IconButton
						onClick={() => forward()}
						disabled={!canGoForward}
						title="Forward"
						aria-label="Forward"
					>
						<ArrowRight className="h-3.5 w-3.5" />
					</IconButton>
				</>
			)}
			{!mergedTools && (
				<IconButton onClick={() => bumpKey()} title="Refresh" aria-label="Refresh pane">
					<RefreshCw className="h-3.5 w-3.5" />
				</IconButton>
			)}
			<EffectiveContextMenu
				menuId="address"
				triggerDisabled={!mergedTools || !leaf || !soleTab}
				target={{ resource: soleTabPath, paneKind: soleTab?.kind }}
				conditions={{
					'artifact-tab': soleTab?.kind === 'artifact',
					'artifact-or-route-tab': soleTabPath !== undefined,
				}}
				disabled={(id) => (id === 'tab.close' ? isSoleTabPinned : false)}
				labels={{
					'pin-sidebar': 'Pin to sidebar…',
					'tab.toggle-pin': isSoleTabPinned ? 'Unpin tab' : 'Pin tab',
					'copy-path': 'Copy path',
					'tab.close': 'Close',
				}}
				handlers={addressMenuHandlers}
				builtinsNeedHandler
			>
					<Input
						ref={inputRef}
						type="text"
						value={draft}
						onChange={(e) => {
							setDraft(e.target.value);
							if (invalid) setInvalid(false);
						}}
						onKeyDown={(e) => {
							if (e.key === 'Enter') {
								e.preventDefault();
								submit();
							} else if (e.key === 'Escape') {
								setDraft(address);
								inputRef.current?.blur();
							}
						}}
						// §6A.3: the address text is this tab's drag source when there's
						// no tab strip to carry one. `beginPointerDrag` is threshold-gated
						// (see pointer-drag.ts) so a plain click still places the cursor —
						// only a real drag past the threshold hijacks the pointer.
						onPointerDown={
							mergedTools && leaf
								? (e) =>
										beginPointerDrag(e, {
											label: rowLabel ?? 'Tab',
											onStart: () => useDragState.getState().startPane(leaf.id, 0),
											onEnd: () => useDragState.getState().end(),
										})
								: undefined
						}
						spellCheck={false}
						autoCorrect="off"
						autoCapitalize="off"
						aria-invalid={invalid || undefined}
						aria-label="Address"
						className={cn(
							'pane-address-text ml-1 h-6 flex-1 rounded-sm px-2 py-0 font-mono text-xs',
							invalid && 'border-destructive ring-2 ring-destructive/40'
						)}
					/>
			</EffectiveContextMenu>
			{view.kind === 'artifact' && (
				<>
					<IconButton
						onClick={() => setPinDialogOpen(true)}
						disabled={pinForCurrentPath !== null}
						title={
							pinForCurrentPath
								? `Already pinned as "${pinForCurrentPath.label}"`
								: 'Pin to activity bar'
						}
						aria-label="Pin to activity bar"
					>
						<PinGlyph
							className={cn('h-3.5 w-3.5', pinForCurrentPath && 'fill-current text-amber-500')}
						/>
					</IconButton>
					<PinArtifactDialog
						open={pinDialogOpen}
						onOpenChange={setPinDialogOpen}
						path={view.path}
					/>
				</>
			)}
			{mergedTools && leaf && (
				<>
					<button
						ref={addBtnRef}
						type="button"
						onClick={(e) => {
							e.stopPropagation();
							focusPane(leaf.id);
							setNewTabOpen((v) => !v);
						}}
						title="New tab in pane"
						aria-label="New tab"
						aria-expanded={newTabOpen}
						className="flex h-6 w-6 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-accent-foreground"
					>
						<Plus className="h-3.5 w-3.5" />
					</button>
					<NewTabMenu
						leaf={leaf}
						open={newTabOpen}
						onClose={() => setNewTabOpen(false)}
						anchor={newTabAnchor}
					/>
				</>
			)}
			{mergedTools && (
				<PaneTools
					paneId={paneId}
					onRefresh={bumpKey}
					history={{ canGoBack, canGoForward, back, forward }}
					onPinToArtifacts={view.kind === 'artifact' ? () => setPinDialogOpen(true) : undefined}
				/>
			)}
		</div>
	);
}
