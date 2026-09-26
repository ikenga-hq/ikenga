// The rail (WP-03): Project · Chi · Ngwa · pins · Settings.
//
// Three nouns, one surface each (DEC-1): Project (⌘1) owns the workbench,
// Chi (⌘2) brings the Companion forward, Ngwa (⌘3) is the equipment
// catalogue that absorbed Packages. Below them sit the user's pins (a
// package's rail presence is a pin since WP-22 seeded one per former rail
// icon), then Settings (⌘,) at the foot. ⌘4–⌘6 are retired and unbound.
//
// Keyboard: the rail is one tab stop (roving tabindex, spec §1.4); ↑/↓ move
// between items, Home/End jump, Enter/Space activate. Every item has a
// 400 ms tooltip carrying its key hint from the keymap registry (§1.3) and
// an inset focus ring (§1.2). The first-contact gloss for the lore nouns is
// `rail-gloss.tsx`.
//
// Re-homed out of the rail (see the WP-03 PR's affordance account): the
// pending-approvals badge → the status bar's permissions segment (WP-09;
// meanwhile ⌘K → "Approvals"), the theme toggle → a ⌘K palette action
// (WP-09) and Settings › Appearance. The active-project indicator stays at
// the rail foot until WP-09 lifts it into the title row.

import {
	ArrowDown,
	ArrowUp,
	Folder,
	FolderKanban,
	HeartPulse,
	type LucideIcon,
	Package,
	Pencil,
	Pin as PinGlyph,
	PinOff,
	Plus,
	Settings,
	Settings2,
	SquareDashed,
	Store,
	Trash2,
} from 'lucide-react';
import { createContext, Fragment, useContext, useEffect, useMemo, useRef, useState } from 'react';
import { Button } from '@/components/ui/button';
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuSeparator,
	ContextMenuTrigger,
} from '@/components/ui/context-menu';
import { useEffectiveMenu } from '@/lib/actions/store';
import { resolveMenuItems } from '@/shell/menu/resolve';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from '@/components/ui/tooltip';
import { cn } from '@/components/ui/utils';
import { useIkengaStore } from '@/lib/ikenga/theme-store';
import { useCommands } from '@/lib/keymap/dispatcher';
import { labelFor } from '@/lib/keymap/registry';
import { usePaneStore } from '@/lib/panes/pane-store';
import {
	type PkgActivityBarEntry,
	usePkgActivityBarEntries,
} from '@/lib/pkg/use-activity-bar-entries';
import { useUpdatesAvailable } from '@/lib/registry/use-updates-available';
import {
	computeReorderIds,
	dispatchPinSelection,
	type Pin,
	type Section,
	useActivityBarPins,
	usePinsStore,
} from '@/lib/shell/pins-store';
import { type CoreMode, useShellStore } from '@/lib/shell/shell-store';
import type { Project } from '@/lib/tauri-cmd';
import { focusCompanion } from './companion-focus';
import { PinIcon } from './pin-icon';
import { RailGloss, type RailGlossTerm } from './rail-gloss';

// ─── Rail keys ────────────────────────────────────────────────────────────

/** Glyphs from the locked frame (`designs/frame-workbench-v4.html` `#i-chi`,
 *  `#i-ngwa`), drawn on the same 16-unit grid and stroked with currentColor
 *  so the tint cascade colours them like the lucide glyphs beside them. */
function ChiGlyph({ className }: { className?: string }) {
	return (
		<svg
			viewBox="0 0 16 16"
			fill="none"
			stroke="currentColor"
			strokeWidth={1.4}
			strokeLinecap="round"
			strokeLinejoin="round"
			aria-hidden="true"
			className={className}
		>
			<path d="M8 2v12" />
			<path d="M3.5 5.5 8 8l4.5-2.5" />
			<path d="M3.5 11 8 8.5 12.5 11" />
		</svg>
	);
}

function NgwaGlyph({ className }: { className?: string }) {
	return (
		<svg
			viewBox="0 0 16 16"
			fill="none"
			stroke="currentColor"
			strokeWidth={1.4}
			strokeLinejoin="round"
			aria-hidden="true"
			className={className}
		>
			<path d="M3 3h4.5v4.5H3z" />
			<path d="M8.5 3H13v4.5H8.5z" />
			<path d="M3 8.5h4.5V13H3z" />
			<path d="M8.5 8.5H13V13H8.5z" />
		</svg>
	);
}

interface RailKeyDef {
	mode: CoreMode;
	label: string;
	/** Keymap registry command (src/lib/keymap/defaults.ts, `rail.*`). */
	command: string;
	Icon: LucideIcon | typeof ChiGlyph;
}

const TOP_KEYS: readonly RailKeyDef[] = [
	{ mode: 'project', label: 'Project', command: 'rail.project', Icon: Folder },
	{ mode: 'chi', label: 'Chi', command: 'rail.chi', Icon: ChiGlyph },
	{ mode: 'ngwa', label: 'Ngwa', command: 'rail.ngwa', Icon: NgwaGlyph },
];

const SETTINGS_KEY: RailKeyDef = {
	mode: 'settings',
	label: 'Settings',
	command: 'rail.settings',
	Icon: Settings,
};

/** Landing route per mode, navigated in the focused pane on click / key.
 *  Project and Chi keep whatever the pane shows. */
const MODE_LANDING: Partial<Record<CoreMode, string>> = {
	settings: '/settings/appearance',
	// TODO(WP-10): '/ngwa/installed' (spec §2 ⌘3) once that route exists.
	ngwa: '/claude',
};

/** Ngwa's context menu (spec §3.1 row 3) — the pointer path to what used to
 *  be the Packages rail key. Health is the kernel status page, which also
 *  carries the "parked" state the old per-pkg rail icons showed. */
const NGWA_MENU: ReadonlyArray<{ label: string; to: string; Icon: LucideIcon }> = [
	{ label: 'Installed', to: '/packages', Icon: Package },
	{ label: 'Store', to: '/packages/browse', Icon: Store },
	{ label: 'Health', to: '/pkg-kernel-status', Icon: HeartPulse },
];

/** Tier-1 lore nouns on the rail get the first-contact gloss (spec §1.3,
 *  "Gloss scope: rail keys for Chi and Ngwa only"). Ngwa first, as in the
 *  locked frame; Chi on a later launch. */
function glossTerms(): RailGlossTerm[] {
	return [
		{
			term: 'ngwa',
			text: 'Ngwa — your equipment',
			keyLabel: labelFor('rail.ngwa'),
			anchor: 'ngwa',
		},
		{
			term: 'chi',
			text: 'Chi — your engine session',
			keyLabel: labelFor('rail.chi'),
			anchor: 'chi',
		},
	];
}

// ─── Roving tabindex ─────────────────────────────────────────────────────

interface RovingApi {
	tabIndexFor: (id: string) => 0 | -1;
	onItemFocus: (id: string) => void;
}

const RovingContext = createContext<RovingApi>({
	tabIndexFor: () => 0,
	onItemFocus: () => {},
});

const ROVING_KEYS = new Set(['ArrowDown', 'ArrowUp', 'Home', 'End']);

/** ↑/↓ (wrapping) and Home/End over every `[data-rail-item]` in DOM order.
 *  Exported for the component test. */
export function railKeyTarget(key: string, count: number, index: number): number {
	if (key === 'Home') return 0;
	if (key === 'End') return count - 1;
	if (key === 'ArrowDown') return (index + 1) % count;
	return (index - 1 + count) % count;
}

// ─── The rail ────────────────────────────────────────────────────────────

export function ActivityBar() {
	const activeMode = useShellStore((s) => s.activeMode);
	const setActiveMode = useShellStore((s) => s.setActiveMode);
	const setWorkspace = useIkengaStore((s) => s.setWorkspace);
	const hydratePins = usePinsStore((s) => s.hydrate);
	const { sections, pinsBySection, sectionLessPins, hydrated } = useActivityBarPins();
	const updatesAvailable = useUpdatesAvailable();
	const pinStatus = usePinPkgStatus();
	const railRef = useRef<HTMLElement>(null);
	const [rovingId, setRovingId] = useState<string | null>(null);
	const terms = useMemo(glossTerms, []);

	// Hydrate user pins on first mount. Idempotent — store guards against
	// re-runs and concurrent hydrate calls.
	useEffect(() => {
		void hydratePins();
	}, [hydratePins]);

	// Mirror activeMode → ikenga.workspace, whose DOM sync is the only writer
	// of <html data-workspace> (theme-store.ts). The four modes and the four
	// workspaces are the same nouns, so this is 1:1. `pkg-iframe-host.tsx`
	// observes that attribute and re-pushes the theme into every iframe pkg.
	useEffect(() => {
		setWorkspace(activeMode);
	}, [activeMode, setWorkspace]);

	// Clicking the rail item that's ALREADY active collapses the sidebar, and
	// clicking it again reopens it — the standard editor-rail affordance.
	// Clicking a DIFFERENT item always switches mode and forces the sidebar
	// open, so a collapsed sidebar can never swallow the click and leave the
	// rail highlighted with nothing visible beside it.
	//
	// Returns true when the click was consumed as a pure toggle, so callers
	// skip the mode-switch + navigation they'd otherwise do. Re-navigating on
	// a collapse would yank the focused pane to the landing route as a side
	// effect of what the user reads as "hide this panel".
	function applyRailToggle(isAlreadyActive: boolean): boolean {
		const { sidebarCollapsed, setSidebarCollapsed, toggleSidebar } = useShellStore.getState();
		if (isAlreadyActive) {
			toggleSidebar();
			return true;
		}
		if (sidebarCollapsed) setSidebarCollapsed(false);
		return false;
	}

	function enterMode(mode: CoreMode) {
		setActiveMode(mode);
		const landing = MODE_LANDING[mode];
		if (landing) usePaneStore.getState().navigateFocused(landing);
		// Chi is the only rail key with a second effect: it brings the
		// Companion forward (spec §2 ⌘2). WP-06 listens for this.
		if (mode === 'chi') focusCompanion();
	}

	function handleSelectMode(mode: CoreMode) {
		if (applyRailToggle(activeMode === mode)) return;
		enterMode(mode);
	}

	/** Ngwa context-menu pick: enter Ngwa (without the collapse toggle) and
	 *  open the chosen page in the focused pane. */
	function navigateInNgwa(to: string) {
		if (activeMode !== 'ngwa') {
			const { sidebarCollapsed, setSidebarCollapsed } = useShellStore.getState();
			if (sidebarCollapsed) setSidebarCollapsed(false);
			setActiveMode('ngwa');
		}
		usePaneStore.getState().navigateFocused(to);
	}

	function handleSelectPin(pin: Pin) {
		dispatchPinSelection(pin, usePaneStore.getState());
	}

	// Each key is a registry command (`defaults.ts`, `!inputFocus`) fired by
	// the one key dispatcher (WP-54), which owns the typing guard and every
	// rebind; the rail only says what each does. A key always enters its
	// mode — the collapse toggle is a pointer affordance only.
	useCommands({
		'rail.project': () => enterMode('project'),
		'rail.chi': () => enterMode('chi'),
		'rail.ngwa': () => enterMode('ngwa'),
		'rail.settings': () => enterMode('settings'),
		'ngwa.create': () => navigateInNgwa('/ngwa/create'),
	});

	const hasAnyPins =
		hydrated &&
		(sectionLessPins.length > 0 ||
			Array.from(pinsBySection.values()).some((list) => list.length > 0));

	// Every rail item id, in render order. The roving tab stop is the last
	// item that had focus while it still exists, else the active mode's key.
	const itemIds: string[] = [
		...TOP_KEYS.map((k) => k.mode),
		...(hasAnyPins
			? [
					...sections.flatMap((s) => (pinsBySection.get(s.id) ?? []).map((p) => `pin:${p.id}`)),
					...sectionLessPins.map((p) => `pin:${p.id}`),
				]
			: []),
		SETTINGS_KEY.mode,
		'project-switcher',
	];
	const tabStop = rovingId && itemIds.includes(rovingId) ? rovingId : activeMode;
	const roving: RovingApi = {
		tabIndexFor: (id) => (id === tabStop ? 0 : -1),
		onItemFocus: setRovingId,
	};

	function onRailKeyDown(e: React.KeyboardEvent<HTMLElement>) {
		if (!ROVING_KEYS.has(e.key) || e.altKey || e.ctrlKey || e.metaKey) return;
		const rail = railRef.current;
		if (!rail) return;
		const items = Array.from(rail.querySelectorAll<HTMLElement>('[data-rail-item]'));
		const index = items.indexOf(document.activeElement as HTMLElement);
		if (index < 0 || items.length === 0) return;
		e.preventDefault();
		items[railKeyTarget(e.key, items.length, index)]?.focus();
	}

	return (
		<RovingContext.Provider value={roving}>
			<TooltipProvider delayDuration={400} skipDelayDuration={150}>
				<nav
					ref={railRef}
					aria-label="Activity bar"
					className="ikenga-rail"
					onKeyDown={onRailKeyDown}
				>
					{TOP_KEYS.map((key) =>
						key.mode === 'ngwa' ? (
							<NgwaMenuWrap key={key.mode} onPick={navigateInNgwa}>
								<RailKey
									def={key}
									isActive={activeMode === key.mode}
									onSelect={handleSelectMode}
									badgeCount={updatesAvailable}
								/>
							</NgwaMenuWrap>
						) : (
							<RailKey
								key={key.mode}
								def={key}
								isActive={activeMode === key.mode}
								onSelect={handleSelectMode}
								badgeCount={0}
							/>
						)
					)}

					{hasAnyPins && (
						<>
							<div className="ikenga-rail-rule" aria-hidden="true" />
							<div className="ikenga-rail-pins">
								{sections.map((section) => {
									const list = pinsBySection.get(section.id) ?? [];
									if (list.length === 0) return null;
									return (
										<SectionContextWrap key={section.id} section={section} pinCount={list.length}>
											<div
												className="ikenga-rail-section"
												data-section={section.id}
												title={section.label}
											>
												{list.map((pin, i) => (
													<PinContextWrap
														key={pin.id}
														pin={pin}
														siblings={list}
														index={i}
														allSections={sections}
														onOpen={handleSelectPin}
													>
														<PinButton
															pin={pin}
															status={pinStatus.get(pin.target)}
															onSelect={handleSelectPin}
														/>
													</PinContextWrap>
												))}
											</div>
										</SectionContextWrap>
									);
								})}
								{sectionLessPins.length > 0 && (
									<div className="ikenga-rail-section" data-section="__none" title="Other">
										{sectionLessPins.map((pin, i) => (
											<PinContextWrap
												key={pin.id}
												pin={pin}
												siblings={sectionLessPins}
												index={i}
												allSections={sections}
												onOpen={handleSelectPin}
											>
												<PinButton
													pin={pin}
													status={pinStatus.get(pin.target)}
													onSelect={handleSelectPin}
												/>
											</PinContextWrap>
										))}
									</div>
								)}
							</div>
						</>
					)}

					<div className="ikenga-rail-spacer" />

					<RailKey
						def={SETTINGS_KEY}
						isActive={activeMode === 'settings'}
						onSelect={handleSelectMode}
						badgeCount={0}
					/>

					{/* Active-project indicator — kept at the rail foot until WP-09
					    lifts it into the title row (spec §3.1, §6A.4). */}
					<ProjectIndicator />

					<RailGloss terms={terms} railRef={railRef} />
				</nav>
			</TooltipProvider>
		</RovingContext.Provider>
	);
}

// ─── Items ───────────────────────────────────────────────────────────────

/** The steady-state rail tooltip: label plus the registry's key hint as a
 *  `<kbd>` (spec §1.3). Radix keeps it hoverable, dismissible with Escape and
 *  open while hovered (WCAG 1.4.13). */
function RailTooltip({
	label,
	keyLabel,
	children,
}: {
	label: string;
	keyLabel?: string;
	children: React.ReactNode;
}) {
	return (
		<Tooltip>
			<TooltipTrigger asChild>{children}</TooltipTrigger>
			<TooltipContent side="right" sideOffset={6} data-rail-tooltip="">
				{label}
				{keyLabel ? <kbd className="ikenga-rail-kbd">{keyLabel}</kbd> : null}
			</TooltipContent>
		</Tooltip>
	);
}

interface RailKeyProps extends Omit<React.ComponentPropsWithoutRef<'button'>, 'onSelect'> {
	def: RailKeyDef;
	isActive: boolean;
	onSelect: (m: CoreMode) => void;
	/** Renders a small count pill in the top-right when > 0. */
	badgeCount: number;
}

function RailKey({ def, isActive, onSelect, badgeCount, ...rest }: RailKeyProps) {
	const { tabIndexFor, onItemFocus } = useContext(RovingContext);
	const { Icon, label, mode } = def;
	const plural = badgeCount === 1 ? '' : 's';
	const tipLabel = badgeCount > 0 ? `${label} · ${badgeCount} update${plural}` : label;
	return (
		<RailTooltip label={tipLabel} keyLabel={labelFor(def.command)}>
			<button
				type="button"
				{...rest}
				onClick={() => onSelect(mode)}
				onFocus={() => onItemFocus(mode)}
				tabIndex={tabIndexFor(mode)}
				aria-label={badgeCount > 0 ? `${label} (${badgeCount} update${plural} available)` : label}
				aria-current={isActive ? 'page' : undefined}
				data-rail-item={mode}
				className="ikenga-rail-item"
			>
				<Icon className="h-[18px] w-[18px]" />
				{badgeCount > 0 && (
					<span aria-hidden="true" className="ikenga-rail-badge">
						{badgeCount > 9 ? '9+' : badgeCount}
					</span>
				)}
			</button>
		</RailTooltip>
	);
}

const NGWA_MENU_ICONS: Readonly<Record<string, React.ReactNode>> = {
	'ngwa.installed': <Package className="h-3.5 w-3.5" />,
	'ngwa.store': <Store className="h-3.5 w-3.5" />,
	'ngwa.health': <HeartPulse className="h-3.5 w-3.5" />,
};
const NGWA_MENU_ROUTE: Readonly<Record<string, string>> = {
	'ngwa.installed': NGWA_MENU[0].to,
	'ngwa.store': NGWA_MENU[1].to,
	'ngwa.health': NGWA_MENU[2].to,
};

/** `rail-ngwa` (G-ACTIONS §1.3), resolved only while the menu is open. */
function NgwaMenuWrap({
	onPick,
	children,
}: {
	onPick: (to: string) => void;
	children: React.ReactNode;
}) {
	return (
		<EffectiveContextMenu
			menuId="rail-ngwa"
			icons={NGWA_MENU_ICONS}
			handlers={{
				'ngwa.installed': () => onPick(NGWA_MENU_ROUTE['ngwa.installed']),
				'ngwa.store': () => onPick(NGWA_MENU_ROUTE['ngwa.store']),
				'ngwa.health': () => onPick(NGWA_MENU_ROUTE['ngwa.health']),
			}}
		>
			{children}
		</EffectiveContextMenu>
	);
}

/** Per-pin package state, keyed by route. A pin seeded from a package's
 *  former rail icon (WP-22) carries that icon's badge and "parked" warning
 *  so neither is lost with the per-package rail buttons. Reads only `badge`
 *  / `parked` — never `label`, which the kernel sets to the nav *section*
 *  (Round 8), not the package's name. */
interface PinPkgStatus {
	badge: PkgActivityBarEntry['badge'];
	parked: boolean;
	parkedReason: string | null;
}

function usePinPkgStatus(): ReadonlyMap<string, PinPkgStatus> {
	const { entries } = usePkgActivityBarEntries();
	return useMemo(() => {
		const map = new Map<string, PinPkgStatus>();
		for (const e of entries) {
			map.set(e.route, {
				badge: e.badge ?? null,
				parked: !!e.parked,
				parkedReason: e.parked_reason ?? null,
			});
		}
		return map;
	}, [entries]);
}

interface PinButtonProps extends Omit<React.ComponentPropsWithoutRef<'button'>, 'onSelect'> {
	pin: Pin;
	status?: PinPkgStatus;
	onSelect: (p: Pin) => void;
}

/** Forwards unknown props (and, React 19, `ref`) to the `<button>` so the
 *  `ContextMenuTrigger asChild` / `TooltipTrigger asChild` wrappers can
 *  attach their handlers to it. */
function PinButton({ pin, status, onSelect, ...rest }: PinButtonProps) {
	const { tabIndexFor, onItemFocus } = useContext(RovingContext);
	const id = `pin:${pin.id}`;
	const badge = status?.badge;
	const count = typeof badge?.count === 'number' && badge.count > 0 ? badge.count : 0;
	const detail = [
		status?.parked ? `Parked: ${status.parkedReason ?? 'sidecar stopped'}` : null,
		badge?.tooltip,
	]
		.filter(Boolean)
		.join(' · ');
	return (
		<RailTooltip label={detail ? `${pin.label} · ${detail}` : pin.label}>
			<button
				type="button"
				{...rest}
				onClick={() => onSelect(pin)}
				onFocus={() => onItemFocus(id)}
				tabIndex={tabIndexFor(id)}
				aria-label={detail ? `${pin.label} (${detail})` : pin.label}
				data-rail-item={id}
				data-pin-id={pin.id}
				data-pin-kind={pin.kind}
				className="ikenga-rail-item ikenga-rail-pin"
			>
				<PinIcon iconLucide={pin.iconLucide} iconEmoji={pin.iconEmoji} Fallback={PinGlyph} />
				{status?.parked ? (
					<span aria-hidden="true" className="ikenga-rail-dot" data-tone="danger" />
				) : count > 0 ? (
					<span aria-hidden="true" className="ikenga-rail-badge">
						{count > 9 ? '9+' : count}
					</span>
				) : badge?.dot ? (
					<span aria-hidden="true" className="ikenga-rail-dot" />
				) : null}
			</button>
		</RailTooltip>
	);
}

// ─── Active-project indicator (kept; WP-09 lifts it) ─────────────────────

/** Two-char abbreviation for the activity-bar indicator. Falls back to the
 *  first two visible chars of the display name. */
function projectAbbrev(p: Project): string {
	const name = p.display_name.trim();
	if (!name) return '··';
	// Prefer initials if the user typed multiple words.
	const words = name.split(/\s+/).filter(Boolean);
	if (words.length >= 2) {
		return (words[0]![0]! + words[1]![0]!).toUpperCase();
	}
	return name.slice(0, 2).toUpperCase();
}

function ProjectIndicator() {
	const [open, setOpen] = useState(false);
	const { tabIndexFor, onItemFocus } = useContext(RovingContext);
	const projects = useShellStore((s) => s.projects);
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const setActiveProject = useShellStore((s) => s.setActiveProject);
	const active = projects.find((p) => p.id === activeProjectId);

	// Active first, then non-archived (by position then created_at), then
	// archived last. Mirrors the rule in `/settings/projects` so the popover
	// reads the same as the settings table.
	const sorted = projects.slice().sort((a, b) => {
		if (a.id === activeProjectId) return -1;
		if (b.id === activeProjectId) return 1;
		const aArc = a.archived_at != null ? 1 : 0;
		const bArc = b.archived_at != null ? 1 : 0;
		if (aArc !== bArc) return aArc - bArc;
		if (a.position !== b.position) return a.position - b.position;
		return a.created_at - b.created_at;
	});

	async function pick(id: string) {
		setOpen(false);
		try {
			await setActiveProject(id);
		} catch {
			// Swallowed — the optimistic flip already rolled back in the
			// store on error. A toast surface lands in a later phase.
		}
	}

	function openNewProject() {
		setOpen(false);
		usePaneStore.getState().navigateFocused('/settings/projects');
	}

	const abbrev = active ? projectAbbrev(active) : '··';
	const color = active?.color ?? '#7c7c7c';
	const switchHint = labelFor('palette.projects');
	const title = active
		? `Project: ${active.display_name}${active.root_path ? `\nRoot: ${active.root_path}` : ''}\n(${switchHint} to switch)`
		: `No active project (${switchHint} to switch)`;

	return (
		<Popover open={open} onOpenChange={setOpen}>
			<PopoverTrigger asChild>
				<button
					type="button"
					title={title}
					aria-label={title}
					tabIndex={tabIndexFor('project-switcher')}
					onFocus={() => onItemFocus('project-switcher')}
					data-rail-item="project-switcher"
					className="ikenga-rail-item"
				>
					<span
						aria-hidden
						className="grid h-6 w-6 place-items-center rounded-md border border-border-soft text-[10px] font-semibold text-white"
						style={{ background: color }}
					>
						{abbrev}
					</span>
				</button>
			</PopoverTrigger>
			<PopoverContent side="right" align="end" className="w-64 p-2">
				<div className="px-2 pb-2 pt-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
					Switch project
				</div>
				<ul className="flex max-h-72 flex-col overflow-y-auto">
					{sorted.map((p) => (
						<li key={p.id}>
							<button
								type="button"
								onClick={() => void pick(p.id)}
								className={cn(
									'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm transition-colors',
									'hover:bg-accent hover:text-accent-foreground',
									p.id === activeProjectId && 'bg-accent/60 font-medium',
									p.archived_at != null && 'opacity-60'
								)}
							>
								<span
									aria-hidden
									className="inline-block h-3 w-3 shrink-0 rounded-full border border-border"
									style={{ background: p.color ?? '#7c7c7c' }}
								/>
								{p.icon && <span className="text-sm leading-none">{p.icon}</span>}
								<span className="flex-1 truncate">{p.display_name}</span>
								{p.id === activeProjectId && (
									<span className="text-[10px] uppercase text-muted-foreground">Active</span>
								)}
								{p.archived_at != null && (
									<span className="text-[10px] uppercase text-muted-foreground">Archived</span>
								)}
							</button>
						</li>
					))}
					{sorted.length === 0 && (
						<li className="px-2 py-3 text-center text-xs text-muted-foreground">
							Loading projects…
						</li>
					)}
				</ul>
				<div className="mt-2 border-t border-border pt-2">
					<button
						type="button"
						onClick={openNewProject}
						className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
					>
						<Plus className="h-3.5 w-3.5" />
						New project…
					</button>
					<button
						type="button"
						onClick={openNewProject}
						className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground"
					>
						<FolderKanban className="h-3.5 w-3.5" />
						Manage projects…
					</button>
				</div>
			</PopoverContent>
		</Popover>
	);
}

// ─── Pin + section context menus (kept) ──────────────────────────────────

interface PinContextWrapProps {
	pin: Pin;
	/** The pin's section, in rail order — for Move up / Move down. */
	siblings: readonly Pin[];
	index: number;
	allSections: readonly Section[];
	onOpen: (pin: Pin) => void;
	children: React.ReactNode;
}

/** Right-click menu for a single pin: Open, Move up / Move down (the
 *  single-pointer alternative to drag-reorder, WCAG 2.5.7 — spec §6A.10),
 *  Move to section, Unpin. Cross-section moves put the pin first in the
 *  destination; `/settings/activity-bar` remains the place for finer edits. */
function PinContextWrap({
	pin,
	siblings,
	index,
	allSections,
	onOpen,
	children,
}: PinContextWrapProps) {
	const removePin = usePinsStore((s) => s.removePin);
	const reorderPins = usePinsStore((s) => s.reorderPins);
	const sectionKey = pin.sectionId ?? '';

	async function moveTo(sectionId: string | null) {
		if (sectionId === pin.sectionId) return;
		await reorderPins([pin.id], sectionId ?? '');
	}

	async function moveBy(delta: -1 | 1) {
		const ids = computeReorderIds(siblings, index, index + delta);
		if (ids.length === 0) return;
		await reorderPins(ids, sectionKey);
	}

	const otherSections = allSections.filter((s) => s.id !== pin.sectionId);
	// `rail` (G-ACTIONS §1.3): resolved by `PinMenuBody`, mounted only while
	// this pin's menu is open.
	const [open, setOpen] = useState(false);

	return (
		<ContextMenu onOpenChange={setOpen}>
			<ContextMenuTrigger asChild>{children}</ContextMenuTrigger>
			{open && (
				<PinMenuBody
					pin={pin}
					isFirst={index === 0}
					isLast={index >= siblings.length - 1}
					otherSections={otherSections}
					handlers={{
						'rail.pin-open': () => onOpen(pin),
						'rail.pin-move-up': () => void moveBy(-1),
						'rail.pin-move-down': () => void moveBy(1),
						'rail.pin-no-section': () => void moveTo(null),
						'rail.unpin': () => void removePin(pin.id),
					}}
					onMoveTo={(sectionId) => void moveTo(sectionId)}
				/>
			)}
		</ContextMenu>
	);
}

const PIN_MENU_ICONS: Readonly<Record<string, React.ReactNode>> = {
	'rail.pin-open': <PinGlyph className="h-3.5 w-3.5" />,
	'rail.pin-move-up': <ArrowUp className="h-3.5 w-3.5" />,
	'rail.pin-move-down': <ArrowDown className="h-3.5 w-3.5" />,
	'rail.pin-no-section': <SquareDashed className="h-3.5 w-3.5" />,
	'rail.unpin': <PinOff className="h-3.5 w-3.5" />,
};

function PinMenuBody({
	pin,
	isFirst,
	isLast,
	otherSections,
	handlers,
	onMoveTo,
}: {
	pin: Pin;
	isFirst: boolean;
	isLast: boolean;
	otherSections: readonly Section[];
	handlers: Record<string, () => void>;
	onMoveTo: (sectionId: string) => void;
}) {
	const railMenu = useEffectiveMenu('rail');
	const rows = resolveMenuItems(railMenu, {
		conditions: { 'sectioned-pin': pin.sectionId !== null },
		disabled: (id) => (id === 'rail.pin-move-up' ? isFirst : id === 'rail.pin-move-down' ? isLast : false),
		labels: { 'rail.pin-open': `Open ${pin.label}` },
		icons: PIN_MENU_ICONS,
		handlers,
	});
	if (rows.length === 0) return null;
	return (
		<ContextMenuContent>
			{rows.map((row, i) => {
				if (row.kind === 'separator') {
					// biome-ignore lint/suspicious/noArrayIndexKey: separators are unkeyed structural markers
					return <ContextMenuSeparator key={`sep-${i}`} />;
				}
				if (row.display === 'submenu' && row.id === 'rail.pin-move-to-section') {
					// `rail.pin-move-to-section` (§1.3): a flat, header-divided
					// list of the other sections — not a nested flyout.
					return otherSections.length > 0 ? (
						<Fragment key={row.id}>
							<div className="px-2 pt-1 pb-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
								Move to
							</div>
							{otherSections.map((s) => (
								<ContextMenuItem key={s.id} data-action={row.dataAction} onSelect={() => onMoveTo(s.id)}>
									<SquareDashed className="h-3.5 w-3.5" />
									{s.label}
								</ContextMenuItem>
							))}
						</Fragment>
					) : null;
				}
				return (
					<ContextMenuItem
						key={row.id}
						data-action={row.dataAction}
						disabled={row.disabled}
						title={row.disabledReason}
						variant={row.danger ? 'destructive' : undefined}
						onSelect={row.run}
					>
						{row.icon}
						{row.label}
					</ContextMenuItem>
				);
			})}
		</ContextMenuContent>
	);
}

const RAIL_SECTION_MENU_ICONS: Readonly<Record<string, React.ReactNode>> = {
	'rail.section-rename': <Pencil className="h-3.5 w-3.5" />,
	'rail.section-manage': <Settings2 className="h-3.5 w-3.5" />,
	'rail.section-delete': <Trash2 className="h-3.5 w-3.5" />,
};

interface SectionContextWrapProps {
	section: Section;
	pinCount: number;
	children: React.ReactNode;
}

/** Right-click menu for a section group container. Rename / Delete here;
 *  the settings page (/settings/activity-bar) is the home for richer edits
 *  like icons. */
function SectionContextWrap({ section, pinCount, children }: SectionContextWrapProps) {
	const updateSection = usePinsStore((s) => s.updateSection);
	const removeSection = usePinsStore((s) => s.removeSection);
	const [renameOpen, setRenameOpen] = useState(false);
	const [confirmDelete, setConfirmDelete] = useState(false);
	const [draftLabel, setDraftLabel] = useState(section.label);
	const [renameError, setRenameError] = useState<string | null>(null);

	async function commitRename(e: React.FormEvent) {
		e.preventDefault();
		const trimmed = draftLabel.trim();
		if (!trimmed) {
			setRenameError('Label is required.');
			return;
		}
		if (trimmed === section.label) {
			setRenameOpen(false);
			return;
		}
		try {
			await updateSection({ id: section.id, label: trimmed });
			setRenameError(null);
			setRenameOpen(false);
		} catch (err) {
			setRenameError(err instanceof Error ? err.message : String(err));
		}
	}

	async function handleConfirmDelete() {
		try {
			await removeSection(section.id);
		} catch {
			// pins-store surfaces error; don't crash the UI
		}
		setConfirmDelete(false);
	}

	return (
		<>
			<EffectiveContextMenu
				menuId="rail-section"
				header={
					<div className="px-2 pt-1 pb-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
						Section · {section.label}
					</div>
				}
				icons={RAIL_SECTION_MENU_ICONS}
				handlers={{
					'rail.section-rename': () => {
						setDraftLabel(section.label);
						setRenameError(null);
						setRenameOpen(true);
					},
					'rail.section-manage': () => usePaneStore.getState().navigateFocused('/settings/activity-bar'),
					'rail.section-delete': () => setConfirmDelete(true),
				}}
			>
				{children}
			</EffectiveContextMenu>

			<Dialog open={renameOpen} onOpenChange={setRenameOpen}>
				<DialogContent className="sm:max-w-sm">
					<form onSubmit={commitRename}>
						<DialogHeader>
							<DialogTitle>Rename section</DialogTitle>
							<DialogDescription>
								The section id (<code className="font-mono">{section.id}</code>) doesn't change —
								pins keep their parent.
							</DialogDescription>
						</DialogHeader>
						<div className="mt-4 flex flex-col gap-2">
							<input
								autoFocus
								value={draftLabel}
								onChange={(e) => {
									setDraftLabel(e.target.value);
									if (renameError) setRenameError(null);
								}}
								className="h-9 rounded border border-input bg-background px-3 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
								aria-label="Section label"
							/>
							{renameError && (
								<div className="rounded border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
									{renameError}
								</div>
							)}
						</div>
						<DialogFooter className="mt-6">
							<Button type="button" variant="ghost" onClick={() => setRenameOpen(false)}>
								Cancel
							</Button>
							<Button type="submit">Save</Button>
						</DialogFooter>
					</form>
				</DialogContent>
			</Dialog>

			<Dialog open={confirmDelete} onOpenChange={setConfirmDelete}>
				<DialogContent className="sm:max-w-md">
					<DialogHeader>
						<DialogTitle>Delete section "{section.label}"?</DialogTitle>
						<DialogDescription>
							{pinCount === 0 ? (
								<>This section has no pins. It will be removed.</>
							) : (
								<>
									Its {pinCount} {pinCount === 1 ? 'pin' : 'pins'} will move to{' '}
									<strong>No section</strong> — they won't be deleted.
								</>
							)}
						</DialogDescription>
					</DialogHeader>
					<DialogFooter>
						<Button type="button" variant="ghost" onClick={() => setConfirmDelete(false)}>
							Cancel
						</Button>
						<Button
							type="button"
							className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
							onClick={handleConfirmDelete}
						>
							Delete section
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>
		</>
	);
}
