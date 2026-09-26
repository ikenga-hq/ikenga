import { useEffect, useState } from 'react';
import { PanelGroup } from 'react-resizable-panels';
import { usePaneStore } from '@/lib/panes/pane-store';
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

	// Keyboard map (workspace-level). See Phase 12 spec § Keybinding history
	// for the rationale behind ⌘W / ⌘T moving from PR-A bindings.
	//   ⌘B           → toggle nav rail
	//   ⌘\           → split focused pane right
	//   ⌘⇧\          → split focused pane down
	//   ⌘W           → close focused PANE
	//   ⌘⇧W          → close active tab
	//   ⌘T           → command palette (views mode)
	//   ⌘⇧T          → reopen last-closed view (depth 10)
	//   ⌘P           → command palette (project switcher — Phase 0)
	//   ⌃T           → new bash terminal in focused pane
	//   ⌃⇧T          → new claude terminal in focused pane
	//   ⌃1 .. ⌃6     → focus pane N (DFS leaf order)
	//
	// On non-Mac platforms there's no Cmd key, so `mod` matches Ctrl. That
	// means ⌃T (terminal) and ⌘T (palette) collide on Linux/Win; the
	// ctrlOnly branch fires first and "new bash terminal" wins. Use ⌘K to
	// open the palette in "all" mode on those platforms — same end result.
	useEffect(() => {
		function onKey(e: KeyboardEvent) {
			const target = e.target as HTMLElement | null;
			const inEditable = !!target?.matches('input, textarea, [contenteditable="true"]');
			const mod = e.metaKey || e.ctrlKey;
			const ctrlOnly = e.ctrlKey && !e.metaKey;

			if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'b') {
				e.preventDefault();
				useShellStore.getState().toggleSidebar();
				return;
			}
			if (mod && !e.altKey && e.key === '\\') {
				e.preventDefault();
				usePaneStore.getState().splitFocused(e.shiftKey ? 'vertical' : 'horizontal');
				return;
			}
			if (e.key.toLowerCase() === 't' && !inEditable) {
				// ⌃T / ⌃⇧T → new terminal (matches ctrlOnly first so Linux Ctrl-T
				// still creates a terminal even though `mod` would match too).
				if (ctrlOnly && !e.altKey) {
					e.preventDefault();
					const sessionId = e.shiftKey ? createClaudeTerminalSession() : createTerminalSession();
					const focusedId = usePaneStore.getState().focusedId;
					usePaneStore.getState().addTab(focusedId, { kind: 'terminal', sessionId });
					return;
				}
				// ⌘T → palette views; ⌘⇧T → reopen.
				if (mod && !e.altKey) {
					e.preventDefault();
					if (e.shiftKey) {
						usePaneStore.getState().reopenLastClosed();
					} else {
						palette.setOpen(true, 'views');
					}
					return;
				}
			}
			if (mod && !e.altKey && e.key.toLowerCase() === 'p' && !inEditable) {
				// ⌘P → project switcher (Phase 0 projects-first-class).
				// ⌘⇧P keeps the legacy "open tabs" switcher available — both
				// open the same palette in different modes.
				e.preventDefault();
				palette.setOpen(true, e.shiftKey ? 'switcher' : 'projects');
				return;
			}
			// ⌘⇧N — open the artifact creation wizard from anywhere (Phase C
			// of plans/shell/2026-05-17-projects-and-artifact-wizard.md, D8).
			// The wizard is mounted by the /projects/new-artifact route, so
			// we just navigate the focused pane there.
			if (mod && e.shiftKey && !e.altKey && e.key.toLowerCase() === 'n' && !inEditable) {
				e.preventDefault();
				usePaneStore.getState().navigateFocused('/projects/new-artifact');
				return;
			}
			// Don't intercept ⌘W while typing.
			if (mod && !e.altKey && e.key.toLowerCase() === 'w' && !inEditable) {
				e.preventDefault();
				if (e.shiftKey) {
					usePaneStore.getState().closeActiveTab();
				} else {
					usePaneStore.getState().closeFocusedPane();
				}
				return;
			}
			if (ctrlOnly && !e.shiftKey && !e.altKey && /^[1-6]$/.test(e.key)) {
				e.preventDefault();
				usePaneStore.getState().focusByIndex(parseInt(e.key, 10) - 1);
				return;
			}
			// ⌘J — toggle the Companion (strip ↔ expanded); keymap `companion.toggle`.
			if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'j' && !inEditable) {
				e.preventDefault();
				useCompanionStore.getState().cycleState();
				return;
			}
		}
		window.addEventListener('keydown', onKey);
		return () => window.removeEventListener('keydown', onKey);
	}, [palette]);

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
