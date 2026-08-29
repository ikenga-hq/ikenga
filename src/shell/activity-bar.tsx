import { useQuery } from '@tanstack/react-query';
import {
	Activity,
	AlertTriangle,
	CheckSquare,
	Clock,
	Folder,
	FolderKanban,
	GitBranch,
	GitCommit,
	GitFork,
	GitGraph,
	GitPullRequest,
	Grid3x3,
	Layers,
	LayoutDashboard,
	LayoutGrid,
	ListChecks,
	type LucideIcon,
	Mail,
	Monitor,
	Moon,
	Package,
	Pencil,
	Pin as PinGlyph,
	PinOff,
	Plus,
	Send,
	Settings,
	Settings2,
	SquareDashed,
	SquareTerminal,
	Sun,
	Trash2,
	TrendingUp,
} from 'lucide-react';
import { useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import {
	ContextMenu,
	ContextMenuContent,
	ContextMenuItem,
	ContextMenuSeparator,
	ContextMenuTrigger,
} from '@/components/ui/context-menu';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { cn } from '@/components/ui/utils';
import { type IkengaMode, type IkengaWorkspace, useIkengaStore } from '@/lib/ikenga/theme-store';
import { usePaneStore } from '@/lib/panes/pane-store';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { modeForRoute } from '@/lib/shell/mode-routes';
import {
	type PkgActivityBarEntry,
	usePkgActivityBarEntries,
} from '@/lib/pkg/use-activity-bar-entries';
import { paActionsListQueryOptions } from '@/lib/queries/pa-actions';
import { useUpdatesAvailable } from '@/lib/registry/use-updates-available';
import {
	dispatchPinSelection,
	type Pin,
	type Section,
	useActivityBarPins,
	usePinsStore,
} from '@/lib/shell/pins-store';
import {
	type ActivityMode,
	type CoreMode,
	isPkgMode,
	useShellStore,
} from '@/lib/shell/shell-store';
import type { Project } from '@/lib/tauri-cmd';
import { PinIcon } from './pin-icon';

interface CoreItem {
	mode: CoreMode;
	label: string;
	Icon: LucideIcon;
	shortcut: string;
}

// Post-strip: workspace surfaces up top, system surfaces (packages,
// settings) at the bottom. App pkgs no longer claim rail icons.
// Artifact-grid lives in the top rail because it's the per-project
// authoring surface — same shape as App / Files / Sessions. Uses
// `Grid3x3` so it doesn't collide visually with App's `LayoutGrid`.
const CORE_TOP: CoreItem[] = [
	{ mode: 'app', label: 'App', Icon: LayoutGrid, shortcut: '⌘1' },
	{ mode: 'files', label: 'Files', Icon: Folder, shortcut: '⌘2' },
	{ mode: 'sessions', label: 'Sessions', Icon: SquareTerminal, shortcut: '⌘3' },
	{ mode: 'artifact-grid', label: 'Artifact grid', Icon: Grid3x3, shortcut: '⌘4' },
];

// Packages + Ngwa sit above Settings — system-level surfaces (registry,
// Claude-config management), not per-workspace entries like the top rail.
// Ngwa (⌘6) graduates the Claude-config browser into its own activity-bar
// mode, replacing the old App-mode /claude NavItem. The Layers glyph reads
// as the layered config store (Ọba) it manages.
const CORE_BOTTOM: CoreItem[] = [
	{ mode: 'pkgs', label: 'Packages', Icon: Package, shortcut: '⌘5' },
	{ mode: 'ngwa', label: 'Ngwa', Icon: Layers, shortcut: '⌘6' },
	{ mode: 'settings', label: 'Settings', Icon: Settings, shortcut: '⌘,' },
];

const SHORTCUT_MAP: Record<string, ActivityMode> = {
	'1': 'app',
	'2': 'files',
	'3': 'sessions',
	'4': 'artifact-grid',
	'5': 'pkgs',
	'6': 'ngwa',
	',': 'settings',
};

/** Landing route per mode — used by ⌘N shortcut + click. Settings + Packages
 *  + Ngwa navigate the focused pane; App / Files / Sessions reuse whatever the
 *  user last looked at in that mode (handled by tab-workspace state). Ngwa's
 *  Browse surface (the 2-pane Claude-config list/detail) is owned by WP-07,
 *  which fills the `/claude` route the sidebar deep-links into. */
const MODE_LANDING: Partial<Record<ActivityMode, string>> = {
	settings: '/settings/appearance',
	pkgs: '/packages/browse',
	ngwa: '/claude',
};

// Workspace tint mirrors core mode 1:1 post-strip; no mini-app rollup.
// Dynamic `pkg:<id>` modes claim no workspace tint of their own — they map to
// the neutral 'app' tint so the `[data-workspace]` cascade stays valid.
function modeToWorkspace(mode: ActivityMode): IkengaWorkspace {
	return isPkgMode(mode) ? 'app' : mode;
}

// Map manifest `ui.nav[].icon` strings to LucideIcon components. Pkg authors
// reference icons by name; the shell controls which ones are actually
// available. Unknown names fall back to a generic `Package` glyph.
const PKG_ICONS: Record<string, LucideIcon> = {
	activity: Activity,
	clock: Clock,
	'check-square': CheckSquare,
	'layout-dashboard': LayoutDashboard,
	'list-checks': ListChecks,
	'trending-up': TrendingUp,
	send: Send,
	mail: Mail,
	folder: Folder,
	'folder-kanban': FolderKanban,
	pencil: Pencil,
	package: Package,
	'git-branch': GitBranch,
	'git-fork': GitFork,
	'git-pull-request': GitPullRequest,
	'git-commit': GitCommit,
	'git-graph': GitGraph,
	history: Clock,
	'layout-grid': LayoutGrid,
};

function iconForPkg(name: string | null | undefined): LucideIcon {
	if (name && name in PKG_ICONS) return PKG_ICONS[name]!;
	return Package;
}

export function ActivityBar() {
	const activeMode = useShellStore((s) => s.activeMode);
	const setActiveMode = useShellStore((s) => s.setActiveMode);
	const setWorkspace = useIkengaStore((s) => s.setWorkspace);
	const hydratePins = usePinsStore((s) => s.hydrate);
	const { sections, pinsBySection, sectionLessPins, hydrated } = useActivityBarPins();

	// Hydrate user pins on first mount. Idempotent — store guards against
	// re-runs and concurrent hydrate calls.
	useEffect(() => {
		void hydratePins();
	}, [hydratePins]);

	// Mirror activeMode → ikenga.workspace so the data-workspace attribute on
	// <html> drives all the workspace-tint variables.
	useEffect(() => {
		setWorkspace(modeToWorkspace(activeMode));
	}, [activeMode, setWorkspace]);

	const updatesAvailable = useUpdatesAvailable();
	const { entries: pkgEntries, loaded: pkgEntriesLoaded } = usePkgActivityBarEntries();

	// Reconcile a stale pkg mode. A `pkg:<id>` mode can survive in persisted
	// state after its pkg was uninstalled; once the kernel snapshot has loaded
	// and confirms no matching entry, fall back to 'app' so the sidebar doesn't
	// strand on "Waiting for pkg menu…". Guarded on `pkgEntriesLoaded` so it
	// never fires before the snapshot arrives.
	useEffect(() => {
		if (!pkgEntriesLoaded || !isPkgMode(activeMode)) return;
		const stillInstalled = pkgEntries.some((e) => `pkg:${e.pkg_id}` === activeMode);
		if (stillInstalled) return;
		// Transient guard: during a dev reload the pkg entry briefly leaves the
		// kernel snapshot before re-registering. Don't snap to 'app' while the
		// focused pane is still showing this pkg's route — PkgMode renders a
		// "Waiting for pkg menu…" placeholder in the gap, and the route→mode
		// sync (router-pane-sync) re-asserts the mode once the pkg re-registers.
		// Only reconcile to 'app' once the user has actually navigated away.
		const { root, focusedId } = usePaneStore.getState();
		const leaf = findLeaf(root, focusedId);
		const view = leaf?.tabs[leaf?.activeTabIdx ?? 0];
		const focusedPath = view?.kind === 'route' ? view.path : null;
		if (focusedPath && modeForRoute(focusedPath) === activeMode) return;
		setActiveMode('app');
	}, [pkgEntriesLoaded, pkgEntries, activeMode, setActiveMode]);

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

	function handleSelectPkg(entry: PkgActivityBarEntry) {
		// Each app pkg owns its own activity mode (`pkg:<id>`). Switch to it so
		// the rail icon highlights and the sidebar renders the pkg's runtime
		// menu (Sidebar branches on `isPkgMode`), then navigate the focused pane
		// to the pkg route. App mode keeps its own main nav, untouched.
		if (applyRailToggle(activeMode === `pkg:${entry.pkg_id}`)) return;
		setActiveMode(`pkg:${entry.pkg_id}`);
		usePaneStore.getState().navigateFocused(entry.route);
	}

	function handleSelectMode(mode: ActivityMode) {
		if (applyRailToggle(activeMode === mode)) return;
		setActiveMode(mode);
		const landing = MODE_LANDING[mode];
		if (landing) {
			usePaneStore.getState().navigateFocused(landing);
		}
	}

	function handleSelectPin(pin: Pin) {
		dispatchPinSelection(pin, usePaneStore.getState());
	}

	useEffect(() => {
		function onKey(e: KeyboardEvent) {
			const mod = e.metaKey || e.ctrlKey;
			if (!mod || e.shiftKey || e.altKey) return;
			const target = e.target as HTMLElement | null;
			if (target?.matches('input, textarea, [contenteditable="true"]')) return;
			const next = SHORTCUT_MAP[e.key];
			if (!next) return;
			e.preventDefault();
			setActiveMode(next);
			const landing = MODE_LANDING[next];
			if (landing) {
				usePaneStore.getState().navigateFocused(landing);
			}
		}
		window.addEventListener('keydown', onKey);
		return () => window.removeEventListener('keydown', onKey);
	}, [setActiveMode]);

	const hasAnyPins =
		hydrated &&
		(sectionLessPins.length > 0 ||
			Array.from(pinsBySection.values()).some((list) => list.length > 0));

	return (
		<nav
			aria-label="Activity bar"
			className="flex h-full w-14 shrink-0 flex-col items-center border-r border-border-soft py-3"
			style={{ background: 'var(--bg-base)' }}
		>
			{/* Registered: pkg / nav-config items. Pinned items live below the
          divider so pkg updates and uninstalls don't clobber user pins. */}
			<div className="flex flex-col items-center">
				{CORE_TOP.map((item) => (
					<RailButton
						key={item.mode}
						mode={item.mode}
						label={item.label}
						Icon={item.Icon}
						shortcut={item.shortcut}
						isActive={activeMode === item.mode}
						onSelect={handleSelectMode}
						badgeCount={0}
					/>
				))}
			</div>

			{pkgEntries.length > 0 && (
				<>
					<div
						className="my-2 h-px w-6 shrink-0"
						style={{ background: 'var(--border-soft)' }}
						aria-hidden="true"
					/>
					<div className="flex flex-col items-center" data-section="pkgs">
						{pkgEntries.map((entry) => (
							<PkgRailButton
								key={entry.pkg_id}
								entry={entry}
								isActive={activeMode === `pkg:${entry.pkg_id}`}
								onSelect={handleSelectPkg}
							/>
						))}
					</div>
				</>
			)}

			{hasAnyPins && (
				<>
					<div
						className="my-2 h-px w-6 shrink-0"
						style={{ background: 'var(--border-soft)' }}
						aria-hidden="true"
					/>
					<div className="flex flex-col items-center gap-1 overflow-y-auto">
						{sections.map((section) => {
							const list = pinsBySection.get(section.id) ?? [];
							if (list.length === 0) return null;
							return (
								<SectionContextWrap key={section.id} section={section} pinCount={list.length}>
									<div
										className="flex flex-col items-center"
										data-section={section.id}
										title={section.label}
									>
										{list.map((pin) => (
											<PinContextWrap
												key={pin.id}
												pin={pin}
												allSections={sections}
												onOpen={handleSelectPin}
											>
												<PinButton pin={pin} onSelect={handleSelectPin} />
											</PinContextWrap>
										))}
									</div>
								</SectionContextWrap>
							);
						})}
						{sectionLessPins.length > 0 && (
							<div className="flex flex-col items-center" data-section="__none" title="Other">
								{sectionLessPins.map((pin) => (
									<PinContextWrap
										key={pin.id}
										pin={pin}
										allSections={sections}
										onOpen={handleSelectPin}
									>
										<PinButton pin={pin} onSelect={handleSelectPin} />
									</PinContextWrap>
								))}
							</div>
						)}
					</div>
				</>
			)}

			<div className="mt-auto" />

			<ApprovalsRailButton />

			<ThemeToggleButton />

			{CORE_BOTTOM.map((item) => (
				<RailButton
					key={item.mode}
					mode={item.mode}
					label={item.label}
					Icon={item.Icon}
					shortcut={item.shortcut}
					isActive={activeMode === item.mode}
					onSelect={handleSelectMode}
					badgeCount={item.mode === 'pkgs' ? updatesAvailable : 0}
				/>
			))}

			{/* Active-project indicator — phase 0 (projects-first-class). Sits
			    below the bottom rail (Packages + Settings) so it's the visual
			    floor of the activity bar, distinct from the pinned section
			    that may grow above. */}
			<ProjectIndicator />
		</nav>
	);
}

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
	const title = active
		? `Project: ${active.display_name}${active.root_path ? `\nRoot: ${active.root_path}` : ''}\n(⌘P to switch)`
		: 'No active project (⌘P to switch)';

	return (
		<Popover open={open} onOpenChange={setOpen}>
			<PopoverTrigger asChild>
				<button
					type="button"
					title={title}
					aria-label={title}
					className={cn(
						'relative my-1 grid h-9 w-9 place-items-center rounded-md transition-colors',
						'hover:bg-card'
					)}
					style={{ color: 'var(--fg-faint)' }}
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

interface PkgRailButtonProps {
	entry: PkgActivityBarEntry;
	/** True when this pkg's mode (`pkg:<id>`) is the active activity mode. */
	isActive: boolean;
	onSelect: (entry: PkgActivityBarEntry) => void;
}

/** Rail button for a pkg activity-bar entry. Selecting it switches the shell
 *  into the pkg's own mode (`pkg:<id>`) — see `handleSelectPkg`. Active state
 *  mirrors `RailButton`'s left-bar + raised-bg treatment, but neutral: pkgs
 *  claim no workspace tint, so the active glyph uses `--fg` rather than a
 *  `--tint-*` var. */
function PkgRailButton({ entry, isActive, onSelect }: PkgRailButtonProps) {
	const Icon = iconForPkg(entry.icon);
	const badge = entry.badge;
	const hasCount = typeof badge?.count === 'number' && badge.count > 0;
	const tooltip = [
		entry.parked ? `Parked: ${entry.parked_reason ?? 'sidecar stopped'}` : null,
		badge?.tooltip,
	]
		.filter(Boolean)
		.join(' · ');
	const title = `${entry.label}${tooltip ? ` · ${tooltip}` : ''}`;
	return (
		<button
			type="button"
			onClick={() => onSelect(entry)}
			title={title}
			aria-label={title}
			aria-current={isActive ? 'page' : undefined}
			disabled={entry.parked}
			className={cn(
				'relative my-0.5 grid h-9 w-9 place-items-center rounded-md transition-colors',
				'hover:bg-card',
				entry.parked && 'opacity-60'
			)}
			style={{
				color: isActive ? 'var(--fg)' : 'var(--fg-faint)',
				background: isActive ? 'var(--bg-raised)' : undefined,
			}}
		>
			{isActive && (
				<span
					aria-hidden="true"
					className="absolute -left-0.5 top-2 bottom-2 w-0.5 rounded-r"
					style={{ background: 'var(--fg)' }}
				/>
			)}
			<Icon className="h-[18px] w-[18px]" />
			{entry.parked ? (
				<span
					aria-hidden="true"
					className="absolute right-1 top-1 h-3.5 w-3.5 rounded-full bg-[var(--destructive,#ef4444)] ring-2 grid place-items-center"
					style={{ ['--tw-ring-color' as string]: 'var(--bg-base)' }}
				>
					<AlertTriangle className="h-2.5 w-2.5 text-white" />
				</span>
			) : hasCount ? (
				<span
					aria-hidden="true"
					className="absolute right-1 top-1 grid h-3.5 min-w-[14px] place-items-center rounded-full bg-[var(--accent,#3b82f6)] px-1 text-[9px] font-semibold leading-none text-white"
				>
					{badge && badge.count! > 9 ? '9+' : badge?.count}
				</span>
			) : (
				badge?.dot && (
					<span
						aria-hidden="true"
						className="absolute right-1.5 top-1.5 h-2 w-2 rounded-full ring-2"
						style={{
							background: 'var(--accent,#3b82f6)',
							['--tw-ring-color' as string]: 'var(--bg-base)',
						}}
					/>
				)
			)}
		</button>
	);
}

interface RailButtonProps {
	mode: ActivityMode;
	label: string;
	Icon: LucideIcon;
	shortcut: string;
	isActive: boolean;
	onSelect: (m: ActivityMode) => void;
	/** Renders a small dot/pill in the top-right when > 0. */
	badgeCount: number;
}

function RailButton({
	mode,
	label,
	Icon,
	shortcut,
	isActive,
	onSelect,
	badgeCount,
}: RailButtonProps) {
	const ws = modeToWorkspace(mode);
	const titleSuffix = badgeCount > 0 ? ` · ${badgeCount} update${badgeCount === 1 ? '' : 's'}` : '';
	return (
		<button
			type="button"
			onClick={() => onSelect(mode)}
			title={`${label} (${shortcut})${titleSuffix}`}
			aria-label={badgeCount > 0 ? `${label} (${badgeCount} updates available)` : label}
			aria-current={isActive ? 'page' : undefined}
			data-ws={ws}
			className={cn(
				'relative my-0.5 grid h-9 w-9 place-items-center rounded-md transition-colors',
				'hover:bg-card'
			)}
			style={{
				color: isActive ? `var(--tint-${ws}-fg)` : 'var(--fg-faint)',
				background: isActive ? 'var(--bg-raised)' : undefined,
			}}
		>
			{isActive && (
				<span
					aria-hidden="true"
					className="absolute -left-0.5 top-2 bottom-2 w-0.5 rounded-r"
					style={{ background: `var(--tint-${ws}-fg)` }}
				/>
			)}
			<Icon className="h-[18px] w-[18px]" />
			{badgeCount > 0 && (
				<span
					aria-hidden="true"
					className="absolute right-1 top-1 grid h-3.5 min-w-[14px] place-items-center rounded-full bg-[var(--accent,#3b82f6)] px-1 text-[9px] font-semibold leading-none text-white"
				>
					{badgeCount > 9 ? '9+' : badgeCount}
				</span>
			)}
		</button>
	);
}

/** Standalone activity-bar attention badge for pending approvals. Shows only
 *  when ≥1 draft is awaiting at the approve gate; clicking opens
 *  /outbox/approvals. Polls (15s) so the count stays fresh even if the
 *  pa-action-paused event is missed; the route's commit/reject invalidations
 *  refresh it too. */
function ApprovalsRailButton() {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);
	const { data } = useQuery({ ...paActionsListQueryOptions(), refetchInterval: 15_000 });
	const count = data?.length ?? 0;
	if (count === 0) return null;
	const plural = count === 1 ? '' : 's';
	return (
		<button
			type="button"
			onClick={() => navigateFocused('/outbox/approvals')}
			title={`${count} approval${plural} awaiting`}
			aria-label={`${count} approval${plural} awaiting — open the approve gate`}
			className={cn(
				'relative my-0.5 grid h-9 w-9 place-items-center rounded-md transition-colors',
				'hover:bg-card'
			)}
			style={{ color: 'var(--fg-faint)' }}
		>
			<CheckSquare className="h-[18px] w-[18px]" />
			<span
				aria-hidden="true"
				className="absolute right-1 top-1 grid h-3.5 min-w-[14px] place-items-center rounded-full bg-[var(--live,#22c55e)] px-1 text-[9px] font-semibold leading-none text-white"
			>
				{count > 9 ? '9+' : count}
			</span>
		</button>
	);
}

const MODE_CYCLE: Record<IkengaMode, IkengaMode> = {
	light: 'dark',
	dark: 'system',
	system: 'light',
};

const MODE_ICON: Record<IkengaMode, LucideIcon> = {
	light: Sun,
	dark: Moon,
	system: Monitor,
};

const MODE_LABEL: Record<IkengaMode, string> = {
	light: 'Light',
	dark: 'Dark',
	system: 'System',
};

function ThemeToggleButton() {
	const mode = useIkengaStore((s) => s.mode);
	const setMode = useIkengaStore((s) => s.setMode);
	const Icon = MODE_ICON[mode];
	const next = MODE_CYCLE[mode];
	return (
		<button
			type="button"
			onClick={() => setMode(next)}
			title={`Theme: ${MODE_LABEL[mode]} (click for ${MODE_LABEL[next]})`}
			aria-label={`Theme: ${MODE_LABEL[mode]}`}
			className={cn(
				'relative my-0.5 grid h-9 w-9 place-items-center rounded-md transition-colors',
				'hover:bg-card'
			)}
			style={{ color: 'var(--fg-faint)' }}
		>
			<Icon className="h-[18px] w-[18px]" />
		</button>
	);
}

interface PinButtonProps {
	pin: Pin;
	onSelect: (p: Pin) => void;
}

function PinButton({ pin, onSelect }: PinButtonProps) {
	return (
		<button
			type="button"
			onClick={() => onSelect(pin)}
			title={pin.label}
			aria-label={pin.label}
			data-pin-id={pin.id}
			data-pin-kind={pin.kind}
			className={cn(
				'relative my-0.5 grid h-9 w-9 place-items-center rounded-md transition-colors',
				'hover:bg-card'
			)}
			style={{ color: 'var(--fg-faint)' }}
		>
			<PinIcon iconLucide={pin.iconLucide} iconEmoji={pin.iconEmoji} Fallback={PinGlyph} />
		</button>
	);
}

interface PinContextWrapProps {
	pin: Pin;
	allSections: readonly Section[];
	onOpen: (pin: Pin) => void;
	children: React.ReactNode;
}

/** Right-click menu for a single pin. Open / Unpin / Move-to-section
 *  (inline list of all sections + No section). Move calls reorderPins
 *  with the pin id solo at sort_order 0 in the destination section —
 *  good enough for v0; the settings page is the place for finer ordering. */
function PinContextWrap({ pin, allSections, onOpen, children }: PinContextWrapProps) {
	const removePin = usePinsStore((s) => s.removePin);
	const reorderPins = usePinsStore((s) => s.reorderPins);

	async function moveTo(sectionId: string | null) {
		if (sectionId === pin.sectionId) return;
		await reorderPins([pin.id], sectionId ?? '');
	}

	const otherSections = allSections.filter((s) => s.id !== pin.sectionId);

	return (
		<ContextMenu>
			<ContextMenuTrigger asChild>{children}</ContextMenuTrigger>
			<ContextMenuContent>
				<ContextMenuItem onSelect={() => onOpen(pin)}>
					<PinGlyph className="h-3.5 w-3.5" />
					Open {pin.label}
				</ContextMenuItem>
				<ContextMenuSeparator />
				{otherSections.length > 0 && (
					<>
						<div className="px-2 pt-1 pb-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
							Move to
						</div>
						{otherSections.map((s) => (
							<ContextMenuItem key={s.id} onSelect={() => moveTo(s.id)}>
								<SquareDashed className="h-3.5 w-3.5" />
								{s.label}
							</ContextMenuItem>
						))}
					</>
				)}
				{pin.sectionId !== null && (
					<ContextMenuItem onSelect={() => moveTo(null)}>
						<SquareDashed className="h-3.5 w-3.5" />
						No section
					</ContextMenuItem>
				)}
				{(otherSections.length > 0 || pin.sectionId !== null) && <ContextMenuSeparator />}
				<ContextMenuItem variant="destructive" onSelect={() => removePin(pin.id)}>
					<PinOff className="h-3.5 w-3.5" />
					Unpin
				</ContextMenuItem>
			</ContextMenuContent>
		</ContextMenu>
	);
}

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
			<ContextMenu>
				<ContextMenuTrigger asChild>{children}</ContextMenuTrigger>
				<ContextMenuContent>
					<div className="px-2 pt-1 pb-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
						Section · {section.label}
					</div>
					<ContextMenuItem
						onSelect={() => {
							setDraftLabel(section.label);
							setRenameError(null);
							setRenameOpen(true);
						}}
					>
						<Pencil className="h-3.5 w-3.5" />
						Rename…
					</ContextMenuItem>
					<ContextMenuItem
						onSelect={() => {
							usePaneStore.getState().navigateFocused('/settings/activity-bar');
						}}
					>
						<Settings2 className="h-3.5 w-3.5" />
						Manage in Settings
					</ContextMenuItem>
					<ContextMenuSeparator />
					<ContextMenuItem variant="destructive" onSelect={() => setConfirmDelete(true)}>
						<Trash2 className="h-3.5 w-3.5" />
						Delete section…
					</ContextMenuItem>
				</ContextMenuContent>
			</ContextMenu>

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
