import {
	ArrowLeft,
	ArrowRight,
	MoreHorizontal,
	RefreshCw,
	SplitSquareHorizontal,
	SplitSquareVertical,
	X,
} from 'lucide-react';
import { labelFor } from '@/lib/keymap/registry';
import type { PaneId, PaneView } from '@/lib/panes/types';
import { usePaneStore } from '@/lib/panes/pane-store';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { hasAddressBar } from '@/lib/panes/pane-address';
import { IconButton } from '@/components/ui/icon-button';
import { useWebviewRoute } from './pane-views';
import { PkgPaneMenuItems, usePkgIdForPane } from './pkg-pane-menu';
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuSeparator,
	DropdownMenuShortcut,
	DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Popover, PopoverTrigger, PopoverContent } from '@/components/ui/popover';
import { writeClipboardText } from '@/lib/transport';
import { pkgWebviewClearSession } from '@/lib/tauri-cmd';
import { useState } from 'react';
import { cn } from '@/components/ui/utils';

interface PaneToolbarProps {
	paneId: PaneId;
	/** Overrides the `⟳` action. The merged address-bar row (§6A.3) passes its
	 *  own history-aware refresh (`bumpKey`) instead of the default
	 *  `refreshPane` remount, so there's exactly one refresh control, not two. */
	onRefresh?: () => void;
	/** Address-bearing views only: folds Back / Forward into the `⋯` menu
	 *  instead of two more always-visible icons in the merged row — keeping
	 *  the resting control count down to the reserved `⟳ + ⋯` pair (a
	 *  four-pane resting layout only shows one focused pane's tools; the
	 *  v4 reference figure is 12 pane-chrome controls). */
	history?: {
		canGoBack: boolean;
		canGoForward: boolean;
		back: () => void;
		forward: () => void;
	};
}

// §a11y line (WCAG 1.4.11, non-text contrast ≥ 3:1): IconButton's shared
// default hover (`hover:bg-accent`, i.e. `--color-accent` → `--bg-raised`)
// sits too close to the pane's own background in the dark Dusk Wood theme
// (measured ~1.17:1 by `e2e/panes.spec.ts`'s "a11y line" check) to register
// as a real non-text indicator. Scoped to just the two pane-tools buttons
// (not a shared `icon-button.tsx` change, which is outside this WP's Files
// list and would ripple into every other IconButton call site) — an
// arbitrary-value Tailwind class so `cn`/`twMerge` drops IconButton's own
// `hover:bg-accent`/`hover:text-accent-foreground` instead of losing to them
// on cascade order.
const PANE_TOOLS_HOVER = 'hover:bg-[color-mix(in_srgb,var(--fg)_65%,transparent)] hover:text-background';

// §a11y line (WCAG 1.4.11, non-text contrast ≥ 3:1) — focus-visible state.
// IconButton's shared `focus-visible:ring-ring` (→ `--primary`, the Dusk
// Wood rust) measured 2.74:1 against the pane background when composited
// (rgb(130,72,43) on rgb(12,10,8)) — below the 3:1 floor the hover fix
// above already targets for these same two buttons. Reuses the identical
// `color-mix(in_srgb, var(--fg) 65%, transparent)` swatch already proven at
// 7.06:1 non-text contrast for the hover state (settled value, measured with
// the button's transition removed via reduced-motion emulation so the read
// can't land mid-transition — see e2e/panes.spec.ts's "a11y line" test), so
// the focus ring and
// the hover background read as one consistent indicator color rather than
// introducing a third color. Only the ring *color* changes here — geometry
// (width, inset) stays whatever IconButton's own
// `focus-visible:ring-2 focus-visible:ring-inset` already sets.
const PANE_TOOLS_FOCUS = 'focus-visible:ring-[color-mix(in_srgb,var(--fg)_65%,transparent)]';

// The reserved pane-tools slot (§6A.1): exactly two buttons, `⟳` and `⋯`.
// Everything the old toolbar exposed as its own always-visible icon (split
// right, split down, close pane) now lives inside the `⋯` menu. The slot
// itself never reflows the row — it renders in both states, fading with
// `opacity` (no `display`/`visibility` swap, so a hovering pointer or a
// tabbed-in focus never loses its target mid-reveal, and a tooltip that was
// already open stays hoverable — WCAG 1.4.13).
//
// Reveal rule (§6A.1): visible on the focused pane always; on any other pane
// while *that pane* is hovered or contains focus. The parent `Pane` sets
// `group/pane` + `data-focused` on its root so this can react to either.
export function PaneTools({ paneId, onRefresh, history }: PaneToolbarProps) {
	const splitPane = usePaneStore((s) => s.splitPane);
	const closePane = usePaneStore((s) => s.closePane);
	const refreshPane = usePaneStore((s) => s.refreshPane);
	const canSplit = usePaneStore((s) => s.canSplit());
	const leafCount = usePaneStore((s) => s.leafCount());
	const activeTab = usePaneStore((s) => {
		const leaf = findLeaf(s.root, paneId);
		return leaf?.tabs[leaf.activeTabIdx];
	});

	const splitDisabled = !canSplit;
	const splitTitle = splitDisabled ? 'Max 6 panes' : undefined;
	const closeDisabled = leafCount <= 1;
	const canCopyPath = Boolean(activeTab && hasAddressBar(activeTab));
	// WP-45: pkg views get the D-08 `pkg-view` menu branch.
	const pkgId = usePkgIdForPane(paneId);
	const reload = () => (onRefresh ? onRefresh() : refreshPane(paneId));

	return (
		<div
			className={cn(
				'pane-tools flex items-center gap-0.5',
				'opacity-0 transition-opacity motion-reduce:transition-none',
				'group-hover/pane:opacity-100 group-focus-within/pane:opacity-100 group-data-[focused=true]/pane:opacity-100',
				'focus-within:opacity-100'
			)}
		>
			<WebviewSessionControl view={activeTab} paneId={paneId} />
			<IconButton
				onClick={reload}
				title="Refresh pane content"
				aria-label="Refresh pane"
				className={cn(PANE_TOOLS_HOVER, PANE_TOOLS_FOCUS)}
			>
				<RefreshCw className="h-3.5 w-3.5" />
			</IconButton>
			<DropdownMenu>
				<DropdownMenuTrigger asChild>
					<IconButton
						title="More pane actions"
						aria-label="More pane actions"
						aria-haspopup="menu"
						className={cn(PANE_TOOLS_HOVER, PANE_TOOLS_FOCUS)}
					>
						<MoreHorizontal className="h-3.5 w-3.5" />
					</IconButton>
				</DropdownMenuTrigger>
				<DropdownMenuContent align="end" className="w-56">
					{history && (
						<>
							<DropdownMenuItem disabled={!history.canGoBack} onSelect={() => history.back()}>
								<ArrowLeft className="h-3.5 w-3.5" />
								Back
							</DropdownMenuItem>
							<DropdownMenuItem disabled={!history.canGoForward} onSelect={() => history.forward()}>
								<ArrowRight className="h-3.5 w-3.5" />
								Forward
							</DropdownMenuItem>
							<DropdownMenuSeparator />
						</>
					)}
					<DropdownMenuItem
						disabled={splitDisabled}
						title={splitTitle}
						onSelect={() => splitPane(paneId, 'horizontal')}
					>
						<SplitSquareHorizontal className="h-3.5 w-3.5" />
						Split right
						<DropdownMenuShortcut>{labelFor('pane.split-right')}</DropdownMenuShortcut>
					</DropdownMenuItem>
					<DropdownMenuItem
						disabled={splitDisabled}
						title={splitTitle}
						onSelect={() => splitPane(paneId, 'vertical')}
					>
						<SplitSquareVertical className="h-3.5 w-3.5" />
						Split down
						<DropdownMenuShortcut>{labelFor('pane.split-down')}</DropdownMenuShortcut>
					</DropdownMenuItem>
					{canCopyPath && activeTab && (activeTab.kind === 'artifact' || activeTab.kind === 'route') && (
						<>
							<DropdownMenuSeparator />
							<DropdownMenuItem
								onSelect={() => void writeClipboardText(activeTab.path).catch(() => {})}
							>
								Copy path
							</DropdownMenuItem>
						</>
					)}
					{pkgId && <PkgPaneMenuItems paneId={paneId} pkgId={pkgId} onReload={reload} />}
					<DropdownMenuSeparator />
					<DropdownMenuItem
						disabled={closeDisabled}
						title={closeDisabled ? 'Cannot close last pane' : undefined}
						onSelect={() => closePane(paneId)}
						variant="destructive"
					>
						<X className="h-3.5 w-3.5" />
						Close pane
						<DropdownMenuShortcut>{labelFor('pane.close')}</DropdownMenuShortcut>
					</DropdownMenuItem>
				</DropdownMenuContent>
			</DropdownMenu>
		</div>
	);
}

// Kept as a pre-existing, always-visible control (not part of the §6A.1
// `⟳ + ⋯` tools slot — it predates this WP and isn't in the list of items
// that moved into `⋯`). Only ever renders for a webview-backed tab, so it
// doesn't add to the resting control count on the common (non-webview) pane.
function WebviewSessionControl({ view, paneId }: { view: PaneView | undefined; paneId: PaneId }) {
	const webviewEntry = useWebviewRoute(view);
	const [isOpen, setIsOpen] = useState(false);
	const [persistence, setPersistence] = useState<'keep' | 'clear-on-exit' | 'ask'>('ask');

	if (!webviewEntry) return null;

	const handleClearSession = async () => {
		setIsOpen(false);
		await pkgWebviewClearSession(webviewEntry.pkg_id, paneId);
	};

	return (
		<Popover open={isOpen} onOpenChange={setIsOpen}>
			<PopoverTrigger asChild>
				<button
					title={`Session: ${persistence}`}
					aria-label={`Session: ${persistence}`}
					className={cn(
						'relative flex h-[26px] w-[26px] items-center justify-center rounded-[4px] text-muted-foreground transition-all hover:bg-muted hover:text-foreground',
						isOpen && 'bg-muted text-foreground'
					)}
				>
					{persistence === 'keep' && (
						<svg
							viewBox="0 0 24 24"
							aria-hidden="true"
							className="h-[14px] w-[14px] fill-none stroke-current stroke-[1.75px] [stroke-linecap:round] [stroke-linejoin:round]"
						>
							<path d="M6 8h12v9a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4z" />
							<path d="M5 5h14v3H5z" />
							<path d="M9 13h6" />
						</svg>
					)}
					{persistence === 'clear-on-exit' && (
						<svg
							viewBox="0 0 24 24"
							aria-hidden="true"
							className="h-[14px] w-[14px] fill-none stroke-current stroke-[1.75px] [stroke-linecap:round] [stroke-linejoin:round]"
						>
							<path d="M6 8h12v9a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4z" strokeDasharray="2.5 2.5" />
							<path d="M5 5h14v3H5z" />
							<path d="m9.5 12.5 5 5" />
							<path d="m14.5 12.5-5 5" />
						</svg>
					)}
					{persistence === 'ask' && (
						<svg
							viewBox="0 0 24 24"
							aria-hidden="true"
							className="h-[14px] w-[14px] fill-none stroke-current stroke-[1.75px] [stroke-linecap:round] [stroke-linejoin:round]"
						>
							<path d="M6 8h12v9a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4z" />
							<path d="M5 5h14v3H5z" />
							<path d="M10.4 13.2a1.7 1.7 0 1 1 2.1 1.9v1.1" />
							<path d="M12.5 18.2h.01" />
						</svg>
					)}
					<span
						className={cn(
							'absolute bottom-[1px] right-[1px] h-[7px] w-[7px] rounded-full border-[1.5px] border-background',
							persistence === 'keep' && 'bg-[#30a46c]', // var(--live) equivalent
							persistence === 'clear-on-exit' && 'bg-[#d97706]', // var(--achievement) equivalent
							persistence === 'ask' && 'bg-[#3b82f6]' // var(--info) equivalent
						)}
					/>
				</button>
			</PopoverTrigger>
			<PopoverContent align="end" className="w-72 p-4">
				<h4 className="mb-1 text-[11.5px] font-semibold">Session</h4>
				<div className="mb-3 font-mono text-[11px] text-muted-foreground break-all">
					{webviewEntry.pkg_id}
				</div>

				<div className="flex flex-col gap-1" role="radiogroup">
					<PersistenceOption
						selected={persistence === 'keep'}
						onClick={() => setPersistence('keep')}
						title="Keep signed in"
						desc="Cookies persist across restarts."
						colorClass="border-[#30a46c] after:bg-[#30a46c]"
					/>
					<PersistenceOption
						selected={persistence === 'clear-on-exit'}
						onClick={() => setPersistence('clear-on-exit')}
						title="Clear on exit"
						desc="Jar is wiped when Ikenga quits."
						colorClass="border-[#d97706] after:bg-[#d97706]"
					/>
					<PersistenceOption
						selected={persistence === 'ask'}
						onClick={() => setPersistence('ask')}
						title="Ask each time"
						desc="Prompt on first open of a session."
						colorClass="border-[#3b82f6] after:bg-[#3b82f6]"
					/>
				</div>

				<div className="my-3 -mx-4 h-px bg-border" />

				<button
					onClick={handleClearSession}
					className="flex h-8 w-full items-center justify-center gap-2 rounded-md border border-destructive/20 bg-destructive/10 text-[11.5px] font-semibold text-destructive transition-colors hover:border-destructive hover:bg-destructive hover:text-destructive-foreground"
				>
					<svg
						viewBox="0 0 24 24"
						className="h-[13px] w-[13px] fill-none stroke-current stroke-[1.75px] [stroke-linecap:round] [stroke-linejoin:round]"
					>
						<path d="M3 6h18" />
						<path d="M8 6V4h8v2" />
						<path d="M19 6l-1 14H6L5 6" />
					</svg>
					Clear session now
				</button>
				<p className="mt-2 text-[11px] leading-tight text-muted-foreground">
					Wipes <code>webjars/{webviewEntry.pkg_id}/default/</code> after the webview is destroyed.
					Forces re-login.
				</p>
			</PopoverContent>
		</Popover>
	);
}

function PersistenceOption({
	selected,
	onClick,
	title,
	desc,
	colorClass,
}: {
	selected: boolean;
	onClick: () => void;
	title: string;
	desc: string;
	colorClass: string;
}) {
	return (
		<button
			role="radio"
			aria-checked={selected}
			onClick={onClick}
			className={cn(
				'flex w-full cursor-pointer gap-3 rounded-md border border-transparent p-2 px-3 text-left transition-colors hover:bg-muted',
				selected && 'border-border bg-muted'
			)}
		>
			<span
				className={cn(
					"mt-[3px] flex h-[13px] w-[13px] shrink-0 items-center justify-center rounded-full border-[1.5px] border-muted-foreground/30 transition-colors after:h-1.5 after:w-1.5 after:rounded-full after:content-['']",
					selected && colorClass
				)}
			/>
			<div>
				<b className="block text-[11.5px] font-semibold">{title}</b>
				<small className="mt-[1px] block text-[11px] leading-tight text-muted-foreground">{desc}</small>
			</div>
		</button>
	);
}
