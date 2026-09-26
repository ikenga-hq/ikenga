import { useState } from 'react';
import { PanelGroup } from 'react-resizable-panels';
import { useCommands } from '@/lib/keymap/dispatcher';
import { findLeaf, getLeafIdsInOrder } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useFilesStore } from '@/lib/shell/files-store';
import { persistPanelSizes } from '@/lib/shell/panel-sizes';
import { useShellStore } from '@/lib/shell/shell-store';
import { createClaudeTerminalSession, createTerminalSession } from '@/terminal/single-terminal';
import { CommandPalette, useCommandPalette } from './command-palette';
import { useCompanionStore } from './companion/companion-store';
import { BannerSlot } from './slots/banner-slot';
import { CompanionSlot } from './slots/companion-slot';
import { IframePoolOverlay, RestOverlays } from './slots/overlay-slot';
import { PaneTreeSlot } from './slots/pane-tree-slot';
import { RailSlot } from './slots/rail-slot';
import { SidebarSlot } from './slots/sidebar-slot';
import { StatusBarSlot } from './slots/status-bar-slot';
import { TitleRowSlot } from './slots/title-row-slot';
import { PostRestartUpdateToast } from './updater/post-restart-toast';
import { UpdateSheet } from './updater/update-sheet';
import { useWorkspaceEffects } from './workspace-effects';

/** New terminal tab in the focused pane (⌃T / ⌃⇧T). */
function addTerminalTab(claude: boolean): void {
	const sessionId = claude ? createClaudeTerminalSession() : createTerminalSession();
	const { focusedId, addTab } = usePaneStore.getState();
	addTab(focusedId, { kind: 'terminal', sessionId });
}

/** Previous / next tab of the focused pane, wrapping (`pane.tab-prev/next`). */
function cycleFocusedTab(delta: 1 | -1): void {
	const { root, focusedId, switchTab } = usePaneStore.getState();
	const leaf = findLeaf(root, focusedId);
	if (!leaf || leaf.tabs.length < 2) return;
	const n = leaf.tabs.length;
	switchTab(focusedId, (((leaf.activeTabIdx + delta) % n) + n) % n);
}

/** Move pane focus to the previous / next leaf in reading (DFS) order —
 *  the same order ⌃1…⌃6 / Alt+1…6 number panes (`pane.focus-up/down`). */
function moveFocusedPane(delta: 1 | -1): void {
	const { root, focusedId, focusPane } = usePaneStore.getState();
	const ids = getLeafIdsInOrder(root);
	const i = ids.indexOf(focusedId);
	const next = ids[i + delta];
	if (i >= 0 && next) focusPane(next);
}

/** The workspace's command handlers (the `workspace` owner in
 *  `lib/keymap/commands.ts`). */
export const WORKSPACE_COMMANDS: Readonly<Record<string, () => void>> = {
	'explorer.toggle': () => useShellStore.getState().toggleSidebar(),
	'explorer.toggle-hidden': () => useFilesStore.getState().toggleShowHidden(),
	'pane.split-right': () => usePaneStore.getState().splitFocused('horizontal'),
	'pane.split-down': () => usePaneStore.getState().splitFocused('vertical'),
	'pane.new-shell-terminal': () => addTerminalTab(false),
	'pane.new-claude-terminal': () => addTerminalTab(true),
	// ⌘⇧N — the artifact creation wizard, mounted by the
	// /projects/new-artifact route (plans/shell/2026-05-17-projects-and-
	// artifact-wizard.md, D8).
	'pane.new-artifact': () => usePaneStore.getState().navigateFocused('/projects/new-artifact'),
	'pane.reopen': () => usePaneStore.getState().reopenLastClosed(),
	'pane.close': () => usePaneStore.getState().closeFocusedPane(),
	'tab.close': () => usePaneStore.getState().closeActiveTab(),
	'pane.focus-1': () => usePaneStore.getState().focusByIndex(0),
	'pane.focus-2': () => usePaneStore.getState().focusByIndex(1),
	'pane.focus-3': () => usePaneStore.getState().focusByIndex(2),
	'pane.focus-4': () => usePaneStore.getState().focusByIndex(3),
	'pane.focus-5': () => usePaneStore.getState().focusByIndex(4),
	'pane.focus-6': () => usePaneStore.getState().focusByIndex(5),
	'pane.tab-prev': () => cycleFocusedTab(-1),
	'pane.tab-next': () => cycleFocusedTab(1),
	'pane.focus-up': () => moveFocusedPane(-1),
	'pane.focus-down': () => moveFocusedPane(1),
	'companion.toggle': () => useCompanionStore.getState().cycleState(),
	'companion.focus-dispatch': () => useCompanionStore.getState().focusDispatch(),
};

export function Workspace() {
	const [initialSizes, setInitialSizes] = useState<[number, number] | null>(null);
	// Sidebar visibility is shared state (see shell-store): ⌘B and the
	// activity-bar rail both drive it, so it can't live as local state here.
	const navHidden = useShellStore((s) => s.sidebarCollapsed);
	const palette = useCommandPalette();

	// App-level hooks/effects (iyke sync/bridge, router/pane sync, projects
	// sync, preload, pa-actions, boot-timing, panel-size hydrate, os-file-drop,
	// pane-tree rehydrate). See workspace-effects.ts for the moved bodies, in
	// their original order and with their original deps.
	useWorkspaceEffects(setInitialSizes);

	// Persist on layout change (debounced to avoid hammering SQLite while
	// the user is mid-drag).
	const persist = (sizes: number[]) => {
		persistPanelSizes(sizes);
	};

	// Keyboard map (workspace-level): every pane / tab / Explorer / Companion
	// key is a registry command (`defaults.ts`) fired by the one key
	// dispatcher (WP-54, DEC-56) — rebindable in `keybindings.json`, never
	// matched here. This only registers what each command does, for as long
	// as the workspace is mounted. The palette's own keys (⌘K, ⌘P, ⌘T, …)
	// are registered by `useCommandPalette()`.
	useCommands(WORKSPACE_COMMANDS);

	if (!initialSizes) {
		return (
			<div
				role="status"
				aria-live="polite"
				className="flex h-screen w-screen items-center justify-center bg-background text-sm text-muted-foreground"
			>
				Loading workspace…
			</div>
		);
	}

	return (
		<div className="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground">
			<TitleRowSlot />
			<BannerSlot />
			{/* WP-41 (D-07 update-flow) — the one sheet + the post-restart toast,
			 * mounted once here rather than inside <BannerSlot />: both need to
			 * render regardless of which (or whether any) update banner is the
			 * one currently shown, so they can't live inside the eligibility-gated
			 * banner wrappers. See src/shell/updater/. */}
			<UpdateSheet />
			<PostRestartUpdateToast />
			<div className="flex min-h-0 flex-1">
				<RailSlot />
				<PanelGroup
					direction="horizontal"
					className="flex-1"
					onLayout={(sizes) => persist(sizes)}
					autoSaveId="ikenga-workspace-v2"
				>
					{!navHidden && <SidebarSlot defaultSize={initialSizes[0]} />}

					<PaneTreeSlot defaultSize={navHidden ? 100 : initialSizes[1]} />
				</PanelGroup>

				<CompanionSlot />
			</div>

			<IframePoolOverlay />

			<CommandPalette
				open={palette.open}
				mode={palette.mode}
				onOpenChange={(open) => palette.setOpen(open)}
			/>

			<RestOverlays />
			<StatusBarSlot />
		</div>
	);
}
