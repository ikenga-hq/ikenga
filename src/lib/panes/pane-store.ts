import { create } from 'zustand';
import { type PaneDirection, type PaneId, type PaneNode, type PaneView, MAX_LEAVES } from './types';
import {
	addTab,
	closeLeaf,
	closeTab,
	findLeaf,
	getLeafIdsInOrder,
	leafCount,
	makeLeaf,
	moveTab,
	navigateFocused,
	replaceActiveTab,
	reorderTab,
	setSplitSizes,
	setTabPinned,
	splitLeaf,
	splitLeafAt,
	switchTab,
	updateTab,
	type MoveTabMode,
} from './pane-reducer';
import { MAX_CLOSED_HISTORY, type PaneTreeSnapshot } from './pane-persistence';

/** Per-pane navigation history. Used by the URL bar in pane-toolbar to
 * provide back/forward across path-bearing view kinds (route, artifact).
 * `entries` is an ordered list of views the user has been on; `index`
 * points at the current one. Not persisted — restarts on reload. */
export interface PaneHistory {
	entries: PaneView[];
	index: number;
}

interface PaneStoreState {
	root: PaneNode;
	focusedId: PaneId;
	/** Stack of recently-closed views; top of stack = most recent. */
	closedHistory: PaneView[];
	/** Per-pane refresh counter. Bumping it forces the pane's content to
	 * re-mount via React `key`. Not persisted — restarts from 0 on reload. */
	refreshTicks: Record<PaneId, number>;
	/** Per-pane navigation history (for the URL bar's back/forward). */
	history: Record<PaneId, PaneHistory>;
	/** Bumped whenever a file (artifact) view is *opened* into a pane — via
	 * `addTab`/`placeView`, not tab-switch or focus changes. The Files sidebar
	 * subscribes to the `nonce` to reveal + scroll-to the file in its tree.
	 * Not persisted. */
	revealRequest: { path: string; nonce: number } | null;
	/** Request the Files sidebar to reveal + expand to this path in the workspace tree. */
	revealPath: (path: string) => void;

	splitFocused: (direction: PaneDirection) => void;
	splitPane: (id: PaneId, direction: PaneDirection) => void;
	closePane: (id: PaneId) => void;
	closeFocusedPane: () => void;
	focusPane: (id: PaneId) => void;
	focusByIndex: (idx: number) => void;
	addTab: (id: PaneId, view: PaneView) => void;
	/** Same as addTab but does not switch focus to the new tab. */
	addTabBackground: (id: PaneId, view: PaneView) => void;
	closeTab: (id: PaneId, tabIdx: number) => void;
	closeActiveTab: () => void;
	switchTab: (id: PaneId, tabIdx: number) => void;
	setTabPinned: (id: PaneId, tabIdx: number, pinned: boolean) => void;
	toggleTabPinned: (id: PaneId, tabIdx: number) => void;
	navigateFocused: (path: string) => void;
	setSplitSizes: (path: number[], sizes: number[]) => void;
	moveTab: (srcLeafId: PaneId, srcTabIdx: number, dstLeafId: PaneId, mode: MoveTabMode) => void;
	/** Reorder a tab within the same leaf. */
	reorderTab: (leafId: PaneId, fromIdx: number, toIdx: number) => void;
	/**
	 * Place an externally-sourced view into the pane tree (e.g., from the
	 * Dock). `'append'` adds it as a tab; edge modes split the dst pane and
	 * place the view in the new sibling leaf. Returns true on success so the
	 * caller can decide whether to remove the view from its source.
	 */
	placeView: (dstLeafId: PaneId, view: PaneView, mode: MoveTabMode) => boolean;

	/** Pop the most-recently-closed view and add it as a tab in the focused pane. */
	reopenLastClosed: () => void;
	/** Bump the refresh tick for a pane (focused if omitted). Re-mounts content. */
	refreshPane: (paneId?: PaneId) => void;
	/** Replace the entire store from a persisted snapshot. Workspace mount only. */
	hydrate: (snapshot: PaneTreeSnapshot) => void;

	/** Record a forward navigation in the pane's history (truncates any
	 * forward stack and appends `view`). Idempotent: repeated identical
	 * pushes don't grow the history. */
	pushHistory: (paneId: PaneId, view: PaneView) => void;
	/** Replace the active view of a pane and append a history entry. Used
	 * by the URL bar: typing a new address swaps the leaf's active tab. */
	replaceActiveViewAndPushHistory: (paneId: PaneId, view: PaneView) => void;
	/** Set or clear the attached terminal tab id on an `artifact-studio`
	 *  view. Mutates in place — does NOT push history (attachment is not
	 *  navigation, see D11). No-op if the active tab isn't an
	 *  `artifact-studio` view. */
	setStudioAttachedTerminal: (paneId: PaneId, tabId: string | null) => void;
	/** Move history index back. Returns the new current view, or null if
	 * already at the oldest entry. */
	historyBack: (paneId: PaneId) => PaneView | null;
	/** Move history index forward. Returns the new current view, or null
	 * if already at the newest entry. */
	historyForward: (paneId: PaneId) => PaneView | null;

	// Derived helpers — fine to expose, callers use them in render.
	leafCount: () => number;
	canSplit: () => boolean;
	leafIdsInOrder: () => PaneId[];
	focusedView: () => PaneView | null;
}

function initialState(): { root: PaneNode; focusedId: PaneId } {
	const initialPath = typeof window !== 'undefined' ? window.location.pathname || '/' : '/';
	const root = makeLeaf({ kind: 'route', path: initialPath });
	return { root, focusedId: root.id };
}

function viewsMatch(a: PaneView, b: PaneView): boolean {
	if (a.kind !== b.kind) return false;
	switch (a.kind) {
		case 'route':
			return a.path === (b as Extract<PaneView, { kind: 'route' }>).path;
		case 'terminal':
			return a.sessionId === (b as Extract<PaneView, { kind: 'terminal' }>).sessionId;
		case 'artifact':
			return a.path === (b as Extract<PaneView, { kind: 'artifact' }>).path;
		case 'artifact-studio': {
			// Match on path + density + vs so that a density transition (grid ↔
			// loupe ↔ compare) or swap of the compare sibling pushes a new
			// history entry rather than collapsing onto the previous one.
			//
			// `attachedTerminalId` is intentionally NOT part of the match:
			// attachment is metadata, not identity. Detach must not push a
			// history entry; dedup must not split two Studio panes that
			// happen to point at the same artifact with different terminals.
			const bb = b as Extract<PaneView, { kind: 'artifact-studio' }>;
			return a.path === bb.path && a.density === bb.density && (a.vs ?? null) === (bb.vs ?? null);
		}
		case 'scratchpad': {
			const bb = b as Extract<PaneView, { kind: 'scratchpad' }>;
			return a.scope === bb.scope && a.name === bb.name;
		}
	}
}

/** The file path of a view that represents a file on disk, else null. Used to
 *  fire a reveal request when such a view is opened into a pane. */
function artifactPath(view: PaneView): string | null {
	if (view.kind === 'artifact' || view.kind === 'artifact-studio') return view.path;
	return null;
}

function findExistingTab(root: PaneNode, leafId: PaneId, view: PaneView): number {
	const leaf = findLeaf(root, leafId);
	if (!leaf) return -1;
	return leaf.tabs.findIndex((t) => viewsMatch(t, view));
}

/**
 * Search every leaf in the tree for a tab matching `view`. Returns the first
 * hit (deterministic order via getLeafIdsInOrder), or null if none found.
 * Used to dedup across panes — clicking a file/route already open in *any*
 * pane should focus that tab instead of creating a duplicate elsewhere.
 */
function findExistingTabAnywhere(
	root: PaneNode,
	view: PaneView
): { leafId: PaneId; tabIdx: number } | null {
	for (const id of getLeafIdsInOrder(root)) {
		const leaf = findLeaf(root, id);
		if (!leaf) continue;
		const idx = leaf.tabs.findIndex((t) => viewsMatch(t, view));
		if (idx >= 0) return { leafId: id, tabIdx: idx };
	}
	return null;
}

function pushClosed(stack: PaneView[], views: PaneView[]): PaneView[] {
	const next = [...stack, ...views];
	return next.length > MAX_CLOSED_HISTORY ? next.slice(next.length - MAX_CLOSED_HISTORY) : next;
}

/** Every terminal sessionId still referenced by a live view anywhere in the
 *  tree — either a `terminal` tab (its `sessionId`) or an `artifact-studio`
 *  view that has a terminal attached (`attachedTerminalId`). Used to decide
 *  whether a just-closed terminal's PTY can be safely disposed. Mirrors
 *  iframe-pool's `liveTabUids`. */
function referencedTerminalIds(root: PaneNode): Set<string> {
	const ids = new Set<string>();
	const visit = (node: PaneNode): void => {
		if (node.type === 'leaf') {
			for (const t of node.tabs) {
				if (t.kind === 'terminal') ids.add(t.sessionId);
				else if (t.kind === 'artifact-studio' && t.attachedTerminalId)
					ids.add(t.attachedTerminalId);
			}
		} else {
			for (const c of node.children) visit(c);
		}
	};
	visit(root);
	return ids;
}

/** Shed the resources tied to the given views when they are GENUINELY CLOSED
 *  (not split/moved/switched — those keep the view alive elsewhere in the
 *  tree). Two concerns:
 *
 *  1. `artifact-studio` views with an attached terminal → `detachFromStudio`
 *     so the side pane re-mounts the xterm. Ownership only; the terminal tab
 *     itself survives (it can still be open in the side pane), so its PTY is
 *     NOT touched here.
 *  2. `terminal` views → `remove(sessionId)` drops the tab, which cascades to
 *     `evictXtermCache` via the session-store subscription in xterm-host
 *     (shedding the live WebglAddon — F-3), and `disposePty(sessionId)` kills
 *     the leaked PTY. The PTY is disposed ONLY when no other live view still
 *     references the session (a terminal can be shared: opened in a second
 *     tab, or attached to an `artifact-studio` pane).
 *
 *  Cross-store: imports lazily to dodge cycles and to keep unit tests
 *  mockable. IMPORTANT — every caller runs `releaseAttachments(...)` BEFORE
 *  the `set()` that commits the new tree, but the work below runs in the
 *  dynamic-import microtask, i.e. AFTER `set()`. So the liveness scan reads
 *  the POST-close tree (the closed views are already gone from it) — no need
 *  to exclude them explicitly. */
function releaseAttachments(views: PaneView[]): void {
	const studioAttachedIds: string[] = [];
	const terminalSessionIds: string[] = [];
	for (const v of views) {
		if (v.kind === 'artifact-studio' && v.attachedTerminalId) {
			studioAttachedIds.push(v.attachedTerminalId);
		} else if (v.kind === 'terminal') {
			terminalSessionIds.push(v.sessionId);
		}
	}
	if (studioAttachedIds.length === 0 && terminalSessionIds.length === 0) return;

	// Lazy imports: neither module imports pane-store at top level, but we
	// still avoid the coupling so unit tests can mock cleanly.
	void Promise.all([import('@/terminal/session-store'), import('@/terminal/pty-registry')]).then(
		([{ useTerminalStore }, { disposePty }]) => {
			const st = useTerminalStore.getState();
			// 1) Studio detach — ownership only, unchanged behavior.
			for (const id of studioAttachedIds) st.detachFromStudio(id);
			if (terminalSessionIds.length === 0) return;
			// 2) Remove each closed terminal tab → cascades evictXtermCache.
			for (const id of terminalSessionIds) st.remove(id);
			// 3) Kill the PTY only when nothing live still points at the session.
			//    Scan reads the post-`set()` tree (see the doc note above).
			const stillLive = referencedTerminalIds(usePaneStore.getState().root);
			for (const id of terminalSessionIds) {
				if (!stillLive.has(id)) disposePty(id);
			}
		}
	);
}

export const usePaneStore = create<PaneStoreState>((set, get) => ({
	...initialState(),
	closedHistory: [],
	refreshTicks: {},
	history: {},
	revealRequest: null,
	revealPath: (path) => {
		set((s) => ({ revealRequest: { path, nonce: (s.revealRequest?.nonce ?? 0) + 1 } }));
	},

	splitFocused: (direction) => {
		const { root, focusedId } = get();
		const r = splitLeaf(root, focusedId, direction);
		if (!r.ok || !r.newLeafId) return;
		set({ root: r.root, focusedId: r.newLeafId });
	},

	splitPane: (id, direction) => {
		const { root } = get();
		const r = splitLeaf(root, id, direction);
		if (!r.ok || !r.newLeafId) return;
		set({ root: r.root, focusedId: r.newLeafId });
	},

	closePane: (id) => {
		const { root, focusedId, closedHistory } = get();
		const leaf = findLeaf(root, id);
		const r = closeLeaf(root, id, focusedId);
		if (!r.ok) return;
		if (leaf) releaseAttachments(leaf.tabs);
		set({
			root: r.root,
			focusedId: r.focusedId,
			closedHistory: leaf ? pushClosed(closedHistory, leaf.tabs) : closedHistory,
		});
	},

	closeFocusedPane: () => {
		const { root, focusedId, closedHistory } = get();
		const leaf = findLeaf(root, focusedId);
		const r = closeLeaf(root, focusedId, focusedId);
		if (!r.ok) return;
		if (leaf) releaseAttachments(leaf.tabs);
		set({
			root: r.root,
			focusedId: r.focusedId,
			closedHistory: leaf ? pushClosed(closedHistory, leaf.tabs) : closedHistory,
		});
	},

	focusPane: (id) => {
		const { root } = get();
		if (!findLeaf(root, id)) return;
		set({ focusedId: id });
	},

	focusByIndex: (idx) => {
		const ids = getLeafIdsInOrder(get().root);
		if (idx < 0 || idx >= ids.length) return;
		set({ focusedId: ids[idx] });
	},

	addTab: (id, view) => {
		const ap = artifactPath(view);
		if (ap) set((s) => ({ revealRequest: { path: ap, nonce: (s.revealRequest?.nonce ?? 0) + 1 } }));
		const { root, focusedId } = get();
		// Resolve to a real leaf. Callers occasionally pass a stale id captured
		// in a closure from before a split/close — without this fallback the
		// reducer's leafId match silently no-ops and the click vanishes.
		const targetId = findLeaf(root, id)
			? id
			: findLeaf(root, focusedId)
				? focusedId
				: (getLeafIdsInOrder(root)[0] ?? id);

		// Prefer the same pane: if it's already there, switch to it and update view (e.g. line/col).
		const sameLeaf = findExistingTab(root, targetId, view);
		if (sameLeaf >= 0) {
			set({ root: updateTab(root, targetId, sameLeaf, view) });
			return;
		}
		// Otherwise, look across all panes — focus the pane that already holds
		// it instead of duplicating (and update view if needed e.g. line/col).
		const elsewhere = findExistingTabAnywhere(root, view);
		if (elsewhere) {
			set({
				root: updateTab(root, elsewhere.leafId, elsewhere.tabIdx, view),
				focusedId: elsewhere.leafId,
			});
			return;
		}
		set({ root: addTab(root, targetId, view) });
	},

	addTabBackground: (id, view) => {
		const { root } = get();
		const sameLeaf = findExistingTab(root, id, view);
		if (sameLeaf >= 0) {
			if (view.kind === 'artifact' && (view.line !== undefined || view.col !== undefined)) {
				set({ root: updateTab(root, id, sameLeaf, view) });
			}
			return;
		}
		const elsewhere = findExistingTabAnywhere(root, view);
		if (elsewhere) {
			if (view.kind === 'artifact' && (view.line !== undefined || view.col !== undefined)) {
				set({ root: updateTab(root, elsewhere.leafId, elsewhere.tabIdx, view) });
			}
			return;
		}
		const leafBefore = findLeaf(root, id);
		const prevActive = leafBefore?.activeTabIdx ?? 0;
		const next = addTab(root, id, view);
		set({ root: switchTab(next, id, prevActive) });
	},

	closeTab: (id, tabIdx) => {
		const { root, focusedId, closedHistory } = get();
		const leaf = findLeaf(root, id);
		const closingView = leaf?.tabs[tabIdx];
		const r = closeTab(root, id, tabIdx, focusedId);
		if (!r.ok) return;
		if (closingView) releaseAttachments([closingView]);
		set({
			root: r.root,
			focusedId: r.focusedId,
			closedHistory: closingView ? pushClosed(closedHistory, [closingView]) : closedHistory,
		});
	},

	closeActiveTab: () => {
		const { root, focusedId, closedHistory } = get();
		const leaf = findLeaf(root, focusedId);
		if (!leaf) return;
		const closingView = leaf.tabs[leaf.activeTabIdx];
		const r = closeTab(root, focusedId, leaf.activeTabIdx, focusedId);
		if (!r.ok) return;
		if (closingView) releaseAttachments([closingView]);
		set({
			root: r.root,
			focusedId: r.focusedId,
			closedHistory: closingView ? pushClosed(closedHistory, [closingView]) : closedHistory,
		});
	},

	switchTab: (id, tabIdx) => {
		set({ root: switchTab(get().root, id, tabIdx) });
	},

	setTabPinned: (id, tabIdx, pinned) => {
		set({ root: setTabPinned(get().root, id, tabIdx, pinned) });
	},

	toggleTabPinned: (id, tabIdx) => {
		const leaf = findLeaf(get().root, id);
		if (!leaf) return;
		const tab = leaf.tabs[tabIdx];
		if (!tab) return;
		set({ root: setTabPinned(get().root, id, tabIdx, !tab.pinned) });
	},

	navigateFocused: (path) => {
		const { root, focusedId } = get();
		set({ root: navigateFocused(root, focusedId, path) });
	},

	setSplitSizes: (path, sizes) => {
		set({ root: setSplitSizes(get().root, path, sizes) });
	},

	moveTab: (srcLeafId, srcTabIdx, dstLeafId, mode) => {
		const { root, focusedId } = get();
		const r = moveTab(root, srcLeafId, srcTabIdx, dstLeafId, mode, focusedId);
		if (!r.ok) return;
		set({ root: r.root, focusedId: r.focusedId });
	},

	reorderTab: (leafId, fromIdx, toIdx) => {
		set({ root: reorderTab(get().root, leafId, fromIdx, toIdx) });
	},

	placeView: (dstLeafId, view, mode) => {
		const ap = artifactPath(view);
		if (ap) set((s) => ({ revealRequest: { path: ap, nonce: (s.revealRequest?.nonce ?? 0) + 1 } }));
		const { root } = get();
		if (!findLeaf(root, dstLeafId)) return false;
		if (mode === 'append') {
			const sameLeaf = findExistingTab(root, dstLeafId, view);
			if (sameLeaf >= 0) {
				set({ root: switchTab(root, dstLeafId, sameLeaf), focusedId: dstLeafId });
				return true;
			}
			const elsewhere = findExistingTabAnywhere(root, view);
			if (elsewhere) {
				set({
					root: switchTab(root, elsewhere.leafId, elsewhere.tabIdx),
					focusedId: elsewhere.leafId,
				});
				return true;
			}
			set({ root: addTab(root, dstLeafId, view), focusedId: dstLeafId });
			return true;
		}
		const direction = mode === 'left' || mode === 'right' ? 'horizontal' : 'vertical';
		const position = mode === 'left' || mode === 'top' ? 'before' : 'after';
		const r = splitLeafAt(root, dstLeafId, direction, view, position);
		if (!r.ok || !r.newLeafId) return false;
		set({ root: r.root, focusedId: r.newLeafId });
		return true;
	},

	reopenLastClosed: () => {
		const { closedHistory, focusedId, root } = get();
		if (closedHistory.length === 0) return;
		const view = closedHistory[closedHistory.length - 1];
		const remaining = closedHistory.slice(0, -1);
		set({
			root: addTab(root, focusedId, view),
			closedHistory: remaining,
		});
	},

	hydrate: (snapshot) => {
		set({
			root: snapshot.root,
			focusedId: snapshot.focusedId,
			closedHistory: snapshot.closedHistory,
		});
	},

	refreshPane: (paneId) => {
		const id = paneId ?? get().focusedId;
		if (!id) return;
		const ticks = get().refreshTicks;
		set({ refreshTicks: { ...ticks, [id]: (ticks[id] ?? 0) + 1 } });
	},

	pushHistory: (paneId, view) => {
		const history = get().history;
		const cur = history[paneId];
		if (cur) {
			const top = cur.entries[cur.index];
			// Idempotent: don't grow history if nothing changed.
			if (top && viewsMatch(top, view)) return;
			const truncated = cur.entries.slice(0, cur.index + 1);
			const entries = [...truncated, view];
			set({
				history: {
					...history,
					[paneId]: { entries, index: entries.length - 1 },
				},
			});
			return;
		}
		set({
			history: { ...history, [paneId]: { entries: [view], index: 0 } },
		});
	},

	setStudioAttachedTerminal: (paneId, tabId) => {
		const { root } = get();
		const leaf = findLeaf(root, paneId);
		if (!leaf) return;
		const active = leaf.tabs[leaf.activeTabIdx];
		if (!active || active.kind !== 'artifact-studio') return;
		// No-op when the value is already what we want. Crucial: otherwise
		// any caller that runs in render or effect with stable args would
		// allocate a fresh tree every render and loop downstream
		// subscribers.
		const current = active.attachedTerminalId ?? null;
		const desired = tabId ?? null;
		if (current === desired) return;
		const next: PaneView = tabId
			? { ...active, attachedTerminalId: tabId }
			: { ...active, attachedTerminalId: undefined };
		const nextRoot = replaceActiveTab(root, paneId, next);
		set({ root: nextRoot });
	},

	replaceActiveViewAndPushHistory: (paneId, view) => {
		const { root, history } = get();
		if (!findLeaf(root, paneId)) return;
		const nextRoot = replaceActiveTab(root, paneId, view);
		const cur = history[paneId];
		let nextHist: PaneHistory;
		if (cur) {
			const top = cur.entries[cur.index];
			if (top && viewsMatch(top, view)) {
				nextHist = cur;
			} else {
				const truncated = cur.entries.slice(0, cur.index + 1);
				const entries = [...truncated, view];
				nextHist = { entries, index: entries.length - 1 };
			}
		} else {
			nextHist = { entries: [view], index: 0 };
		}
		set({ root: nextRoot, history: { ...history, [paneId]: nextHist } });
	},

	historyBack: (paneId) => {
		const { root, history } = get();
		const cur = history[paneId];
		if (!cur || cur.index <= 0) return null;
		const nextIdx = cur.index - 1;
		const view = cur.entries[nextIdx];
		if (!view || !findLeaf(root, paneId)) return null;
		const nextRoot = replaceActiveTab(root, paneId, view);
		set({
			root: nextRoot,
			history: {
				...history,
				[paneId]: { entries: cur.entries, index: nextIdx },
			},
		});
		return view;
	},

	historyForward: (paneId) => {
		const { root, history } = get();
		const cur = history[paneId];
		if (!cur || cur.index >= cur.entries.length - 1) return null;
		const nextIdx = cur.index + 1;
		const view = cur.entries[nextIdx];
		if (!view || !findLeaf(root, paneId)) return null;
		const nextRoot = replaceActiveTab(root, paneId, view);
		set({
			root: nextRoot,
			history: {
				...history,
				[paneId]: { entries: cur.entries, index: nextIdx },
			},
		});
		return view;
	},

	leafCount: () => leafCount(get().root),
	canSplit: () => leafCount(get().root) < MAX_LEAVES,
	leafIdsInOrder: () => getLeafIdsInOrder(get().root),
	focusedView: () => {
		const { root, focusedId } = get();
		const leaf = findLeaf(root, focusedId);
		return leaf ? leaf.tabs[leaf.activeTabIdx] : null;
	},
}));
