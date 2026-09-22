import { useEffect } from 'react';
import { getHomeSync } from '@/lib/home';
import { formatKeyLabel } from '@/lib/keymap/platform';
import { findLeaf, getLeafIdsInOrder } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneId, PaneNode, PaneView } from '@/lib/panes/types';
import { type ActiveProject, useShellStore } from '@/lib/shell/shell-store';
import { type IykeKeymapEntry, iykeSetFrame } from '@/lib/tauri-cmd';
import { type TerminalTab, useTerminalStore } from '@/terminal/session-store';
import { formatTerminalTitle } from '@/terminal/terminal-title';

import { setShell } from './client';
import { getIframe, IFRAME_STATE_EVENT } from './iframe-registry';
import { type KeymapEntry, listKeymap } from './keymap-bridge';

/**
 * Bridge between React shell state and the Iyke Rust mirror. Mounted
 * once inside `<Workspace />` — i.e. only at the global, post-auth
 * level, never inside a pane's memory router.
 *
 * Pushes the active sidebar mode + focused pane's route + a flat
 * leaves-and-tree pane snapshot. Failures are logged, not thrown —
 * Iyke isn't on the user's critical path.
 */
export function useIykeShellSync(): void {
	const activeMode = useShellStore((s) => s.activeMode);
	// Re-fire when focused pane changes or when the tree mutates (any
	// navigation inside a pane updates `root` immutably via the reducer).
	const focusedId = usePaneStore((s) => s.focusedId);
	const root = usePaneStore((s) => s.root);
	// Subscribed so a sidebar collapse/expand pushes immediately. Without
	// this in the dep list the mirror would only catch up on the next
	// unrelated mode/pane change, and `/iyke/state` would report a stale
	// value right after `/iyke/sidebar` — the exact window a caller polls.
	const sidebarCollapsed = useShellStore((s) => s.sidebarCollapsed);
	const terminalTabs = useTerminalStore((s) => s.tabs);

	useEffect(() => {
		pushShellState(activeMode, sidebarCollapsed, root, focusedId, terminalTabs);
	}, [activeMode, focusedId, root, sidebarCollapsed, terminalTabs]);

	// Re-push when a pkg iframe publishes state (selection etc.) so
	// `iyke state` reflects it without waiting for a pane-tree mutation.
	useEffect(() => {
		const onState = () => {
			const shell = useShellStore.getState();
			const panes = usePaneStore.getState();
			pushShellState(
				shell.activeMode,
				shell.sidebarCollapsed,
				panes.root,
				panes.focusedId,
				useTerminalStore.getState().tabs
			);
		};
		window.addEventListener(IFRAME_STATE_EVENT, onState);
		return () => window.removeEventListener(IFRAME_STATE_EVENT, onState);
	}, []);

	// WP-21: `shell.active_project` in `iyke state`. `activeProject` is
	// derived and reference-stable (G-STATE §2), so this fires only when the
	// id, root or extra roots actually change.
	const activeProject = useShellStore((s) => s.activeProject);
	useEffect(() => {
		pushActiveProject(activeProject);
	}, [activeProject]);

	// WP-21: `GET /iyke/keys`. Phase 1's keymap is defaults-only and never
	// changes at runtime, so it is pushed once when the workspace mounts.
	// Phase 6 overrides will need to re-push on change.
	useEffect(() => {
		iykeSetFrame({ keymap: keymapPayload() }).catch((err) => {
			console.warn('[iyke] set_frame (keymap) failed:', err);
		});
	}, []);

	// WP-28: `GET /iyke/explorer/sections`. `explorerSections` is replaced
	// wholesale by reorder/collapse mutations, so the subscription fires
	// exactly on change; the wire shape is the store's
	// `ExplorerSectionState` field-for-field (G-STATE §1).
	const explorerSections = useShellStore((s) => s.explorerSections);
	useEffect(() => {
		iykeSetFrame({ explorerSections }).catch((err) => {
			console.warn('[iyke] set_frame (explorer_sections) failed:', err);
		});
	}, [explorerSections]);
}

function pushActiveProject(activeProject: ActiveProject): void {
	iykeSetFrame({
		activeProject: {
			id: activeProject.id,
			root_path: activeProject.root_path,
			extra_roots: [...activeProject.extra_roots],
		},
	}).catch((err) => {
		console.warn('[iyke] set_frame (active_project) failed:', err);
	});
}

/** The registry rows `GET /iyke/keys` serves: `{command, key, when, source}`
 *  plus the human label and the key hint resolved for this platform. */
export function keymapPayload(entries: KeymapEntry[] = listKeymap()): IykeKeymapEntry[] {
	return entries.map((e) => ({
		command: e.command,
		key: e.key,
		when: e.when,
		source: e.source,
		label: e.label,
		key_label: formatKeyLabel(e.key),
		...(e.platformOnly ? { platform_only: e.platformOnly } : {}),
	}));
}

function pushShellState(
	activeMode: string,
	sidebarCollapsed: boolean,
	root: PaneNode,
	focusedId: PaneId,
	terminalTabs: TerminalTab[]
): void {
	const focused = findLeaf(root, focusedId);
	const view = focused?.tabs[focused.activeTabIdx];
	const route = view?.kind === 'route' ? view.path : null;
	const panes = buildPanesPayload(root, focusedId, terminalTabs);
	setShell({ mode: activeMode, route, panes, sidebarCollapsed }).catch((err) => {
		console.warn('[iyke] set_shell failed:', err);
	});
}

interface LeafSummary {
	id: string;
	focused: boolean;
	activeTabIdx: number;
	tabs: Array<{
		kind: string;
		title: string;
		pinned?: boolean;
		terminalId?: string;
		ptyId?: string;
	}>;
	/** Pkg id when the active tab is a /pkg/<id>/ route. */
	pkg?: string;
	/** Latest state the pkg iframe published (e.g. open-task selection). */
	state?: Record<string, unknown>;
}

interface PanesPayload {
	leaves: LeafSummary[];
	tree: PaneNode;
}

function buildPanesPayload(
	root: PaneNode,
	focusedId: PaneId,
	terminalTabs: TerminalTab[]
): PanesPayload {
	const ids = getLeafIdsInOrder(root);
	const leaves: LeafSummary[] = ids.map((id) => {
		const leaf = findLeaf(root, id);
		if (!leaf) {
			// Defensive — getLeafIdsInOrder pulled this id from the same
			// tree, so findLeaf should always succeed.
			return { id, focused: false, activeTabIdx: 0, tabs: [] };
		}
		const summary: LeafSummary = {
			id,
			focused: id === focusedId,
			activeTabIdx: leaf.activeTabIdx,
			tabs: leaf.tabs.map((t) => {
				const terminal =
					t.kind === 'terminal' ? terminalTabs.find((tab) => tab.id === t.sessionId) : null;
				return {
					kind: t.kind,
					title: viewTitle(t, terminal ?? null),
					...(t.pinned ? { pinned: true } : {}),
					...(t.kind === 'terminal' ? { terminalId: t.sessionId } : {}),
					...(terminal?.ptyId ? { ptyId: terminal.ptyId } : {}),
				};
			}),
		};
		// Surface the pkg id + its latest published state (selection etc.) for
		// pkg-route panes, so external callers can answer "what's open in this
		// pane" from `iyke state` alone. Pkg iframes register by pkg id —
		// see pkg-iframe-host.tsx Step 1c.
		const active = leaf.tabs[leaf.activeTabIdx];
		if (active?.kind === 'route') {
			const m = /^\/pkg\/([^/]+)/.exec(active.path);
			if (m) {
				summary.pkg = m[1];
				const reg = getIframe(m[1]);
				if (reg && Object.keys(reg.state).length > 0) summary.state = reg.state;
			}
		}
		return summary;
	});
	return { leaves, tree: root };
}

function viewTitle(view: PaneView, terminal: TerminalTab | null): string {
	switch (view.kind) {
		case 'route':
			return view.path;
		case 'terminal':
			// A bare session uuid told an agent nothing about which terminal it
			// was looking at. Name it the same way the tab strip does — the
			// foreground command isn't available on this path (no poll here), so
			// this is the spawn-time view; `GET /iyke/terminal/list` remains the
			// live, authoritative surface.
			return terminal
				? formatTerminalTitle({
						cwd: terminal.spec.cwd,
						argv: terminal.spec.cmd,
						title: terminal.title,
						exited: terminal.status === 'exited',
						home: getHomeSync(),
					}).label
				: view.sessionId;
		case 'artifact':
			return view.path;
		case 'artifact-studio':
			return 'artifact-studio';
		case 'scratchpad':
			return `${view.scope}/${view.name}`;
	}
}
