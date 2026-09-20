// WP-20 (frame slot skeleton): app-level hooks/effects extracted verbatim
// from workspace.tsx, in their original order and with their original deps.
//
// The key-listener effect stays in workspace.tsx (Do-not-touch: WP-08's
// keymap registry takes it later). `initialSizes` is JSX/key-listener state
// owned by workspace.tsx (the loading gate reads it, PanelGroup sizes read
// it) so it isn't moved here — the two effects that write it take the
// setter as a parameter instead.
import { type Dispatch, type SetStateAction, useEffect } from 'react';
import { dumpBootTimings, mark } from '@/lib/boot-timing';
import { initOsFileDrop } from '@/lib/dnd/os-file-drop';
import { useIykeBridge } from '@/lib/iyke/bridge';
import { useIykeControlListener } from '@/lib/iyke/control-listener';
import { useIykeShellSync } from '@/lib/iyke/use-iyke-shell-sync';
import { loadPaneTree, persistPaneTree } from '@/lib/panes/pane-persistence';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useRouterPaneSync } from '@/lib/panes/router-pane-sync';
import { loadPanelSizes, registerPanelSizesSetter } from '@/lib/shell/panel-sizes';
import { useProjectsSync } from '@/lib/shell/use-projects-sync';
import { usePaActionsListener } from '@/lib/use-pa-actions';
import { usePreloadViewers } from '@/lib/use-preload-viewers';
import { useScreenshotListener } from '@/lib/use-screenshot-listener';
import { loadClaudeSettingsPath } from '@/terminal/claude-settings';
import { useTerminalStore } from '@/terminal/session-store';

export function useWorkspaceEffects(
	setInitialSizes: Dispatch<SetStateAction<[number, number] | null>>
) {
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
}
