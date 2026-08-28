import { useEffect, useState } from 'react';
import { Panel, PanelGroup, PanelResizeHandle } from 'react-resizable-panels';
import { PkgIframeLayer } from '@/components/pkg/pkg-iframe-layer';
import { dumpBootTimings, mark } from '@/lib/boot-timing';
import { initOsFileDrop } from '@/lib/dnd/os-file-drop';
import { useIykeBridge } from '@/lib/iyke/bridge';
import { loadClaudeSettingsPath } from '@/terminal/claude-settings';
import { useIykeControlListener } from '@/lib/iyke/control-listener';
import { useIykeShellSync } from '@/lib/iyke/use-iyke-shell-sync';

import { IFRAME_POOL_ENABLED } from '@/lib/panes/iframe-pool';
import { loadPaneTree, persistPaneTree } from '@/lib/panes/pane-persistence';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useRouterPaneSync } from '@/lib/panes/router-pane-sync';
import {
	loadPanelSizes,
	persistPanelSizes,
	registerPanelSizesSetter,
} from '@/lib/shell/panel-sizes';
import { useShellStore } from '@/lib/shell/shell-store';
import { useProjectsSync } from '@/lib/shell/use-projects-sync';
import { usePaActionsListener } from '@/lib/use-pa-actions';
import { usePreloadViewers } from '@/lib/use-preload-viewers';
import { useScreenshotListener } from '@/lib/use-screenshot-listener';
import { useTerminalStore } from '@/terminal/session-store';
import { buildClaudeWrappedCmd } from '@/terminal/claude-wrap';
import { createTerminalSession } from '@/terminal/single-terminal';
import { ActivityBar } from './activity-bar';
import { TerminalHandoffPrompt } from './artifact-wizard/terminal-handoff-prompt';
import { WizardPopRecoveryChip } from './artifact-wizard/wizard-pop-recovery-chip';
import { CommandPalette, useCommandPalette } from './command-palette';
import { ConnectionBanner } from './connection-banner';
import { ConnectorBanner } from './connector-banner';
import { ContentPane } from './content-pane';
import { Dock } from './dock/dock';
import { useDockStore } from './dock/dock-store';
import { PkgAutoUpdater } from './pkg-auto-updater';
import { RuntimeBunChip } from './runtime-bun-chip';
import { Sidebar } from './sidebar';
import { TrustReviewBanner } from './trust-review-banner';
import { UpdaterBanner } from './updater-banner';

export function Workspace() {
	const [initialSizes, setInitialSizes] = useState<[number, number] | null>(null);
	// Sidebar visibility is shared state (see shell-store): ⌘B and the
	// activity-bar rail both drive it, so it can't live as local state here.
	const navHidden = useShellStore((s) => s.sidebarCollapsed);
	const palette = useCommandPalette();

	// Iyke (phase 11): mirror sidebar mode + focused pane's route into the
	// Rust-side control bridge so external CLI/MCP callers see what the
	// user sees. Mounted only here so it never fires inside a pane's
	// memory-router re-render.
	useIykeShellSync();
	// Counterpart for the write side: subscribe to iyke:* Tauri events
	// emitted by the Rust handlers and translate them into pane/shell
	// store mutations. Same mounting reasoning — workspace-level only.
	useIykeControlListener();
	// Phase A: console + fetch shims, DOM/click/type/key/wait/query-cache
	// listeners. Mount once at workspace level only.
	useIykeBridge();
	// Warm the `--settings` path so the first `claude` terminal already carries
	// it. `buildAgentArgs` is synchronous, so it can only read a primed value;
	// unprimed it omits the flag and the session loses the shell's live view.
	useEffect(() => {
		void loadClaudeSettingsPath();
	}, []);
	// Bidirectional sync between the address bar and the focused pane's
	// route. Workspace level only — each pane's RouteView memory router
	// stays an internal detail.
	useRouterPaneSync();
	// Screenshot capture bridge: turn `screenshot://request` events into
	// DOM-to-PNG renders, and `screenshot://shortcut` events into Tauri
	// command invocations (resolves "focused pane" client-side).
	useScreenshotListener();
	// Phase 0 (projects-first-class): subscribe to `projects:active-changed`
	// Tauri events and invalidate any `'project-scoped'` TanStack Query so
	// later phases' per-project data swaps automatically on switch.
	useProjectsSync();
	// Warm the lazy artifact viewer chunks during idle so the first
	// PDF/XLSX/code file open isn't a cold fetch.
	usePreloadViewers();
	// Approve-gate seam: when an approve-aware action pauses a batch
	// (`pa-action-paused`), open /outbox/approvals in the focused pane so the
	// operator can sign off the drafts. Workspace-level so any action's pause
	// reliably surfaces the gate.
	usePaActionsListener();
	// Note: the mbox sync scheduler that used to live here moved into the
	// com.ikenga.email pkg's manifest cron when the strip-down landed.

	// Boot-timing checkpoint (see src/lib/boot-timing.ts). Fires once per
	// process — the marks are no-ops on warm reloads.
	useEffect(() => {
		mark('boot:workspace-mount');
	}, []);

	// Hydrate persisted sizes once on mount. One global key — project
	// switches no longer re-load panel sizes.
	useEffect(() => {
		let cancelled = false;
		loadPanelSizes().then((sizes) => {
			if (!cancelled) {
				setInitialSizes(sizes);
				// Workspace is now interactive — log the cold-start trace.
				// Microtask delay so the mark lands after React commits.
				queueMicrotask(() => {
					mark('boot:workspace-ready');
					dumpBootTimings();
				});
			}
		});
		return () => {
			cancelled = true;
		};
	}, []);

	// Setter kept so settings surfaces can push a reset/default back into
	// React state without remounting the panel group.
	useEffect(() => registerPanelSizesSetter(setInitialSizes), []);

	// Route native OS file drops to the surface under the cursor (terminals →
	// insert path). Only fires where the native
	// drag-drop handler is enabled (Linux/Windows); a no-op on macOS. See
	// src/lib/dnd/os-file-drop.ts. (drop-zone overlay)
	useEffect(() => {
		let dispose: (() => void) | null = null;
		let cancelled = false;
		void initOsFileDrop().then((unlisten) => {
			if (cancelled) unlisten();
			else dispose = unlisten;
		});
		return () => {
			cancelled = true;
			dispose?.();
		};
	}, []);

	// Persist on layout change (debounced to avoid hammering SQLite while
	// the user is mid-drag).
	const persist = (sizes: number[]) => {
		persistPanelSizes(sizes);
	};

	// Rehydrate terminal sessions, then the pane tree, then start
	// persisting pane-tree changes. Order matters: pane-persistence
	// checks useTerminalStore for live session ids when filtering
	// restored terminal tabs. Persistence subscriber only attaches after
	// hydrate to avoid clobbering the saved blob with the initial
	// (default) tree.
	useEffect(() => {
		let unsubPersist: (() => void) | null = null;
		let cancelled = false;
		// Race each step against a hard timeout — `rehydrateFromDb` and
		// `loadPaneTree` both hit tauri-plugin-sql's Database.load, which has
		// been observed to silently never resolve. Without this fence the
		// persist subscriber would never attach and pane state would not save.
		const raceTimeout = <T,>(p: Promise<T>, ms: number, label: string): Promise<T | null> =>
			new Promise((resolve) => {
				let done = false;
				const t = setTimeout(() => {
					if (done) return;
					done = true;
					// eslint-disable-next-line no-console
					console.warn(`[workspace] ${label} timed out after ${ms}ms`);
					resolve(null);
				}, ms);
				p.then(
					(v) => {
						if (done) return;
						done = true;
						clearTimeout(t);
						resolve(v);
					},
					(err) => {
						if (done) return;
						done = true;
						clearTimeout(t);
						console.warn(`[workspace] ${label} failed`, err);
						resolve(null);
					}
				);
			});
		void (async () => {
			if (!useTerminalStore.getState().rehydrated) {
				await raceTimeout(
					useTerminalStore.getState().rehydrateFromDb(),
					2000,
					'terminal rehydrate'
				);
			}
			if (cancelled) return;
			const snapshot = await raceTimeout(loadPaneTree(), 2000, 'loadPaneTree');
			if (cancelled) return;
			if (snapshot) usePaneStore.getState().hydrate(snapshot);
			unsubPersist = usePaneStore.subscribe((state) => {
				persistPaneTree({
					root: state.root,
					focusedId: state.focusedId,
					closedHistory: state.closedHistory,
				});
			});
		})();
		return () => {
			cancelled = true;
			if (unsubPersist) unsubPersist();
		};
	}, []);

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
					const sessionId = e.shiftKey
						? createTerminalSession({ cmd: buildClaudeWrappedCmd(), title: 'claude' })
						: createTerminalSession();
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
			// ⌘J — cycle dock state (collapsed → expanded).
			if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'j' && !inEditable) {
				e.preventDefault();
				useDockStore.getState().cycleState();
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
			<ConnectionBanner />
			<UpdaterBanner />
			<PkgAutoUpdater />
			<ConnectorBanner />
			<TrustReviewBanner />
			<div className="flex min-h-0 flex-1">
				<ActivityBar />
				<PanelGroup
					direction="horizontal"
					className="flex-1"
					onLayout={(sizes) => persist(sizes)}
					autoSaveId="ikenga-workspace-v2"
				>
					{!navHidden && (
						<>
							<Panel
								defaultSize={initialSizes[0]}
								minSize={8}
								maxSize={30}
								collapsible
								collapsedSize={6}
							>
								<Sidebar />
							</Panel>
							<PanelResizeHandle
								data-panel-resize-handle-enabled="true"
								aria-label="Resize sidebar"
							/>
						</>
					)}

					<Panel defaultSize={navHidden ? 100 : initialSizes[1]} minSize={40}>
						<ContentPane />
					</Panel>
				</PanelGroup>

				<Dock />
			</div>

			{/* Hoisted iframe pool: owns pooled pkg iframes and floats them
			    (position:fixed, z below every overlay) over their pane rects so
			    they survive tab switch / reorder / split / pane-tree rebuild.
			    Mounted once here, outside the PanelGroup remount scope. Renders
			    nothing when pooling is off (no surfaces are ever claimed). */}
			{IFRAME_POOL_ENABLED && <PkgIframeLayer />}

			<CommandPalette
				open={palette.open}
				mode={palette.mode}
				onOpenChange={(open) => palette.setOpen(open)}
			/>

			<TerminalHandoffPrompt />

			<WizardPopRecoveryChip />
			<RuntimeBunChip />
		</div>
	);
}
