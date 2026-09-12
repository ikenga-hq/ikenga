import {
	type LeafNode,
	type PaneDirection,
	type PaneId,
	type PaneNode,
	type PaneView,
	type SplitNode,
	MAX_LEAVES,
} from './types';

export function newPaneId(): PaneId {
	if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
		return crypto.randomUUID();
	}
	return `p_${Math.random().toString(36).slice(2)}_${Date.now()}`;
}

// --- Per-tab identity -------------------------------------------------
//
// `PaneView` carries no id of its own (content-only: kind + path/sessionId/
// etc.), and its position in `leaf.tabs` isn't stable across reorder/close.
// Rather than adding a real `uid` field to `PaneView` (which would break
// every `toEqual({ kind: 'route', path })`-shaped fixture in
// pane-reducer.test.ts and leak into persisted blobs), identity is tracked
// out-of-band by object reference in a module-scope `WeakMap`. Reducer
// functions preserve the same tab's object reference across reorder
// (`reorderTab` splices, doesn't recreate) and close (`closeTab` filters,
// doesn't recreate), so `tabUid` naturally returns the same id for a tab
// the user only moved. Entries are GC'd for free once a tree revision
// stops referencing the old view object — no manual cleanup needed.
//
// Functions that construct a *new* view object representing the SAME tab
// slot with different content (URL-bar navigate-in-place, pin toggle) call
// `carryTabUid` to propagate the old object's id onto the new one, so that
// content swap doesn't read as "a new tab" to consumers keying by `tabUid`
// (pane.tsx's PaneBody key, route-view.tsx's per-tab router cache).
const tabUidMap = new WeakMap<PaneView, string>();

export function newTabUid(): string {
	if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
		return crypto.randomUUID();
	}
	return `t_${Math.random().toString(36).slice(2)}_${Date.now()}`;
}

/** Stable identity for a tab's view object. Mints and caches on first
 *  access (lazy — a tab gets its id the first time something needs to key
 *  by it, e.g. on mount), then returns the same id for that exact object
 *  reference forever after. Two distinct objects with identical content
 *  (e.g. the same route opened in two tabs of one pane) get distinct ids —
 *  the collision `viewKey` (content-based) has. */
export function tabUid(view: PaneView): string {
	let id = tabUidMap.get(view);
	if (!id) {
		id = newTabUid();
		tabUidMap.set(view, id);
	}
	return id;
}

/** Propagate `from`'s tab identity onto `to`. Use when a reducer function
 *  replaces a tab's view object in place (same slot, new content) so the
 *  replacement isn't treated as a brand-new tab by identity-keyed
 *  consumers. No-op (returns `to` unchanged) if `from` is undefined. */
function carryTabUid(from: PaneView | undefined, to: PaneView): PaneView {
	if (from) tabUidMap.set(to, tabUid(from));
	return to;
}

export function makeLeaf(view: PaneView, id?: PaneId): LeafNode {
	return { type: 'leaf', id: id ?? newPaneId(), tabs: [view], activeTabIdx: 0 };
}

function equalSizes(n: number): number[] {
	if (n <= 0) return [];
	const base = Math.floor(100 / n);
	const sizes = new Array(n).fill(base);
	sizes[n - 1] = 100 - base * (n - 1);
	return sizes;
}

export function leafCount(node: PaneNode): number {
	if (node.type === 'leaf') return 1;
	return node.children.reduce((sum, c) => sum + leafCount(c), 0);
}

export function findLeaf(node: PaneNode, id: PaneId): LeafNode | null {
	if (node.type === 'leaf') return node.id === id ? node : null;
	for (const c of node.children) {
		const r = findLeaf(c, id);
		if (r) return r;
	}
	return null;
}

export function getLeafIdsInOrder(node: PaneNode): PaneId[] {
	if (node.type === 'leaf') return [node.id];
	return node.children.flatMap((c) => getLeafIdsInOrder(c));
}

export function getActiveView(leaf: LeafNode): PaneView {
	return leaf.tabs[leaf.activeTabIdx];
}

function mapLeaves(node: PaneNode, fn: (leaf: LeafNode) => LeafNode): PaneNode {
	if (node.type === 'leaf') return fn(node);
	return { ...node, children: node.children.map((c) => mapLeaves(c, fn)) };
}

export interface SplitResult {
	root: PaneNode;
	newLeafId: PaneId | null;
	ok: boolean;
}

export function splitLeaf(root: PaneNode, leafId: PaneId, direction: PaneDirection): SplitResult {
	if (leafCount(root) >= MAX_LEAVES) {
		return { root, newLeafId: null, ok: false };
	}
	const target = findLeaf(root, leafId);
	if (!target) return { root, newLeafId: null, ok: false };
	const newLeaf = makeLeaf({ kind: 'route', path: '/' });
	const newRoot = applySplit(root, leafId, direction, newLeaf);
	return { root: newRoot, newLeafId: newLeaf.id, ok: true };
}

function applySplit(
	node: PaneNode,
	leafId: PaneId,
	direction: PaneDirection,
	newLeaf: LeafNode
): PaneNode {
	// Target leaf is the root leaf — wrap in a new split.
	if (node.type === 'leaf' && node.id === leafId) {
		return {
			type: 'split',
			direction,
			children: [node, newLeaf],
			sizes: equalSizes(2),
		};
	}
	if (node.type === 'leaf') return node;

	// If this split has the same direction as the requested split AND the
	// target is a direct child, insert as sibling instead of nesting. This
	// prevents pathological deep nesting (matches Zed/VS Code behavior).
	if (node.direction === direction) {
		const idx = node.children.findIndex((c) => c.type === 'leaf' && c.id === leafId);
		if (idx !== -1) {
			const children = [
				...node.children.slice(0, idx + 1),
				newLeaf,
				...node.children.slice(idx + 1),
			];
			return { ...node, children, sizes: equalSizes(children.length) };
		}
	}

	// Recurse.
	return {
		...node,
		children: node.children.map((c) => applySplit(c, leafId, direction, newLeaf)),
	};
}

export interface CloseResult {
	root: PaneNode;
	focusedId: PaneId;
	ok: boolean;
}

export function closeLeaf(root: PaneNode, leafId: PaneId, currentFocusedId: PaneId): CloseResult {
	// Refuse to close the last leaf in the tree — workspace must always have
	// one pane.
	if (leafCount(root) <= 1) {
		return { root, focusedId: currentFocusedId, ok: false };
	}
	const orderBefore = getLeafIdsInOrder(root);
	const targetIdx = orderBefore.indexOf(leafId);
	if (targetIdx === -1) {
		return { root, focusedId: currentFocusedId, ok: false };
	}
	const newRoot = applyClose(root, leafId);
	if (!newRoot) return { root, focusedId: currentFocusedId, ok: false };

	let nextFocus = currentFocusedId;
	if (currentFocusedId === leafId) {
		nextFocus = orderBefore[targetIdx - 1] ?? orderBefore[targetIdx + 1];
	}
	if (!findLeaf(newRoot, nextFocus)) {
		nextFocus = getLeafIdsInOrder(newRoot)[0];
	}
	return { root: newRoot, focusedId: nextFocus, ok: true };
}

function applyClose(node: PaneNode, leafId: PaneId): PaneNode | null {
	if (node.type === 'leaf') return node.id === leafId ? null : node;

	const newChildren: PaneNode[] = [];
	for (const c of node.children) {
		const r = applyClose(c, leafId);
		if (r) newChildren.push(r);
	}

	if (newChildren.length === 0) return null;
	if (newChildren.length === 1) return newChildren[0];

	// Flatten any same-direction nested splits that may have been left
	// behind by collapses inside recursion.
	const flatChildren: PaneNode[] = [];
	for (const c of newChildren) {
		if (c.type === 'split' && c.direction === (node as SplitNode).direction) {
			flatChildren.push(...c.children);
		} else {
			flatChildren.push(c);
		}
	}
	return {
		...node,
		children: flatChildren,
		sizes: equalSizes(flatChildren.length),
	};
}

export function setSplitSizes(root: PaneNode, path: number[], sizes: number[]): PaneNode {
	// Apply user-resize at a specific tree path. `path` is the chain of child
	// indices from root to the target split node.
	if (path.length === 0) {
		if (root.type !== 'split') return root;
		return { ...root, sizes };
	}
	if (root.type !== 'split') return root;
	const [head, ...tail] = path;
	return {
		...root,
		children: root.children.map((c, i) => (i === head ? setSplitSizes(c, tail, sizes) : c)),
	};
}

export function addTab(root: PaneNode, leafId: PaneId, view: PaneView): PaneNode {
	return mapLeaves(root, (leaf) =>
		leaf.id === leafId
			? { ...leaf, tabs: [...leaf.tabs, view], activeTabIdx: leaf.tabs.length }
			: leaf
	);
}

/** Replace the leaf's active tab with `view` (preserving its `pinned` flag).
 * Used by the URL bar to swap the current view without spawning a new tab.
 * No-op if the leaf is not found or has no tabs. */
export function replaceActiveTab(root: PaneNode, leafId: PaneId, view: PaneView): PaneNode {
	return mapLeaves(root, (leaf) => {
		if (leaf.id !== leafId) return leaf;
		if (leaf.tabs.length === 0) return leaf;
		const active = leaf.tabs[leaf.activeTabIdx];
		const next: PaneView = active?.pinned ? { ...view, pinned: true } : view;
		carryTabUid(active, next);
		const tabs = leaf.tabs.map((t, i) => (i === leaf.activeTabIdx ? next : t));
		return { ...leaf, tabs };
	});
}

export function switchTab(root: PaneNode, leafId: PaneId, idx: number): PaneNode {
	return mapLeaves(root, (leaf) => {
		if (leaf.id !== leafId) return leaf;
		if (idx < 0 || idx >= leaf.tabs.length) return leaf;
		return { ...leaf, activeTabIdx: idx };
	});
}

/**
 * Update an existing tab at `tabIdx` with `view` (preserving its `pinned` flag
 * and stable `tabUid`), and switch to it as the active tab.
 */
export function updateTab(
	root: PaneNode,
	leafId: PaneId,
	tabIdx: number,
	view: PaneView
): PaneNode {
	return mapLeaves(root, (leaf) => {
		if (leaf.id !== leafId) return leaf;
		if (tabIdx < 0 || tabIdx >= leaf.tabs.length) return leaf;
		const current = leaf.tabs[tabIdx];
		const next: PaneView = current?.pinned ? { ...view, pinned: true } : view;
		carryTabUid(current, next);
		const tabs = leaf.tabs.map((t, i) => (i === tabIdx ? next : t));
		return { ...leaf, tabs, activeTabIdx: tabIdx };
	});
}

export function setTabPinned(
	root: PaneNode,
	leafId: PaneId,
	tabIdx: number,
	pinned: boolean
): PaneNode {
	return mapLeaves(root, (leaf) => {
		if (leaf.id !== leafId) return leaf;
		if (tabIdx < 0 || tabIdx >= leaf.tabs.length) return leaf;
		const current = leaf.tabs[tabIdx];
		if (Boolean(current.pinned) === pinned) return leaf;
		const updated: PaneView = pinned
			? { ...current, pinned: true }
			: (() => {
					// Strip the property entirely when unpinning so persisted blobs
					// stay terse and equality checks elsewhere don't see a leftover
					// `pinned: false` field.
					const next: Record<string, unknown> = { ...current };
					delete next.pinned;
					return next as PaneView;
				})();
		carryTabUid(current, updated);
		const tabs = leaf.tabs.map((t, i) => (i === tabIdx ? updated : t));
		return { ...leaf, tabs };
	});
}

export interface CloseTabResult {
	root: PaneNode;
	focusedId: PaneId;
	paneClosed: boolean;
	ok: boolean;
}

export function closeTab(
	root: PaneNode,
	leafId: PaneId,
	tabIdx: number,
	currentFocusedId: PaneId
): CloseTabResult {
	const leaf = findLeaf(root, leafId);
	if (!leaf) return { root, focusedId: currentFocusedId, paneClosed: false, ok: false };
	if (tabIdx < 0 || tabIdx >= leaf.tabs.length) {
		return { root, focusedId: currentFocusedId, paneClosed: false, ok: false };
	}
	if (leaf.tabs[tabIdx].pinned) {
		// Pinned tabs stay put. Caller must unpin first.
		return { root, focusedId: currentFocusedId, paneClosed: false, ok: false };
	}
	if (leaf.tabs.length === 1) {
		const r = closeLeaf(root, leafId, currentFocusedId);
		return { ...r, paneClosed: r.ok };
	}
	const tabs = leaf.tabs.filter((_, i) => i !== tabIdx);
	let activeTabIdx = leaf.activeTabIdx;
	if (tabIdx < activeTabIdx) activeTabIdx -= 1;
	else if (tabIdx === activeTabIdx) {
		activeTabIdx = Math.min(activeTabIdx, tabs.length - 1);
	}
	const newRoot = mapLeaves(root, (l) => (l.id === leafId ? { ...l, tabs, activeTabIdx } : l));
	return { root: newRoot, focusedId: currentFocusedId, paneClosed: false, ok: true };
}

/**
 * Like `splitLeaf`, but inserts a fresh leaf with a caller-provided
 * view at the chosen position relative to the target leaf.
 *
 * `position: 'before'` puts the new leaf to the left/top of the target
 * (depending on direction); `'after'` to the right/bottom. Used by the
 * tab DnD machinery so dropping onto the left/top edge of a pane
 * actually places the moved tab on the left/top.
 */
export function splitLeafAt(
	root: PaneNode,
	leafId: PaneId,
	direction: PaneDirection,
	view: PaneView,
	position: 'before' | 'after'
): SplitResult {
	if (leafCount(root) >= MAX_LEAVES) {
		return { root, newLeafId: null, ok: false };
	}
	const target = findLeaf(root, leafId);
	if (!target) return { root, newLeafId: null, ok: false };
	const newLeaf = makeLeaf(view);
	const newRoot = applySplitAt(root, leafId, direction, newLeaf, position);
	return { root: newRoot, newLeafId: newLeaf.id, ok: true };
}

function applySplitAt(
	node: PaneNode,
	leafId: PaneId,
	direction: PaneDirection,
	newLeaf: LeafNode,
	position: 'before' | 'after'
): PaneNode {
	if (node.type === 'leaf' && node.id === leafId) {
		const children = position === 'before' ? [newLeaf, node] : [node, newLeaf];
		return { type: 'split', direction, children, sizes: equalSizes(2) };
	}
	if (node.type === 'leaf') return node;

	if (node.direction === direction) {
		const idx = node.children.findIndex((c) => c.type === 'leaf' && c.id === leafId);
		if (idx !== -1) {
			const insertAt = position === 'before' ? idx : idx + 1;
			const children = [
				...node.children.slice(0, insertAt),
				newLeaf,
				...node.children.slice(insertAt),
			];
			return { ...node, children, sizes: equalSizes(children.length) };
		}
	}

	return {
		...node,
		children: node.children.map((c) => applySplitAt(c, leafId, direction, newLeaf, position)),
	};
}

export type MoveTabMode = 'append' | 'left' | 'right' | 'top' | 'bottom';

export interface MoveTabResult {
	root: PaneNode;
	focusedId: PaneId;
	ok: boolean;
}

/**
 * Move a single tab from one pane to another. `'append'` adds it to
 * the dst pane's tab strip (also used for reorder when src === dst,
 * but only if src has more than one tab). `'left'` / `'right'` /
 * `'top'` / `'bottom'` split the dst pane in the corresponding
 * direction and place the moved tab in a fresh sibling.
 *
 * Refuses on:
 *  - missing src/dst leaves or out-of-range tabIdx
 *  - any non-`'append'` mode while the tree is at MAX_LEAVES
 *  - same-pane any mode when src has only one tab (would be a no-op)
 */
export function moveTab(
	root: PaneNode,
	srcLeafId: PaneId,
	srcTabIdx: number,
	dstLeafId: PaneId,
	mode: MoveTabMode,
	currentFocusedId: PaneId
): MoveTabResult {
	const src = findLeaf(root, srcLeafId);
	if (!src) return { root, focusedId: currentFocusedId, ok: false };
	if (srcTabIdx < 0 || srcTabIdx >= src.tabs.length) {
		return { root, focusedId: currentFocusedId, ok: false };
	}
	if (mode !== 'append' && leafCount(root) >= MAX_LEAVES) {
		return { root, focusedId: currentFocusedId, ok: false };
	}
	if (srcLeafId === dstLeafId && src.tabs.length === 1) {
		return { root, focusedId: currentFocusedId, ok: false };
	}

	const movedView = src.tabs[srcTabIdx];

	// Step 1: remove from src. closeTab handles collapsing if src loses
	// its last tab.
	const removed = closeTab(root, srcLeafId, srcTabIdx, currentFocusedId);
	if (!removed.ok) return { root, focusedId: currentFocusedId, ok: false };
	let newRoot = removed.root;

	// closeTab may collapse splits but leaves stable leaf ids — dst should
	// still be findable. If it isn't, something's wrong; bail.
	if (!findLeaf(newRoot, dstLeafId)) {
		return { root, focusedId: currentFocusedId, ok: false };
	}

	if (mode === 'append') {
		newRoot = mapLeaves(newRoot, (l) =>
			l.id === dstLeafId ? { ...l, tabs: [...l.tabs, movedView], activeTabIdx: l.tabs.length } : l
		);
		return { root: newRoot, focusedId: dstLeafId, ok: true };
	}

	const direction = mode === 'left' || mode === 'right' ? 'horizontal' : 'vertical';
	const position = mode === 'left' || mode === 'top' ? 'before' : 'after';
	const r = splitLeafAt(newRoot, dstLeafId, direction, movedView, position);
	if (!r.ok || !r.newLeafId) {
		return { root, focusedId: currentFocusedId, ok: false };
	}
	return { root: r.root, focusedId: r.newLeafId, ok: true };
}

/**
 * Reorder a tab within a single leaf. `toIdx` is the destination index in
 * the *current* tabs array (insertion semantics: the moved tab ends up at
 * `toIdx` after removal+insert). Out-of-range or no-op moves are returned
 * unchanged. Active tab tracking follows the moved view.
 */
export function reorderTab(
	root: PaneNode,
	leafId: PaneId,
	fromIdx: number,
	toIdx: number
): PaneNode {
	const leaf = findLeaf(root, leafId);
	if (!leaf) return root;
	if (fromIdx < 0 || fromIdx >= leaf.tabs.length) return root;
	const clamped = Math.max(0, Math.min(toIdx, leaf.tabs.length - 1));
	if (clamped === fromIdx) return root;
	return mapLeaves(root, (l) => {
		if (l.id !== leafId) return l;
		const tabs = [...l.tabs];
		const [moved] = tabs.splice(fromIdx, 1);
		tabs.splice(clamped, 0, moved);
		let activeTabIdx = l.activeTabIdx;
		if (l.activeTabIdx === fromIdx) {
			activeTabIdx = clamped;
		} else {
			// Tab between [fromIdx, clamped] (or vice versa) shifts by 1 in the
			// direction opposite to the move.
			if (fromIdx < clamped) {
				if (l.activeTabIdx > fromIdx && l.activeTabIdx <= clamped) activeTabIdx -= 1;
			} else {
				if (l.activeTabIdx >= clamped && l.activeTabIdx < fromIdx) activeTabIdx += 1;
			}
		}
		return { ...l, tabs, activeTabIdx };
	});
}

export function navigateFocused(root: PaneNode, focusedId: PaneId, path: string): PaneNode {
	return mapLeaves(root, (leaf) => {
		if (leaf.id !== focusedId) return leaf;
		// Dedup: if the route is already open in this pane, just switch to it.
		const existing = leaf.tabs.findIndex((t) => t.kind === 'route' && t.path === path);
		if (existing >= 0) {
			return { ...leaf, activeTabIdx: existing };
		}
		const active = leaf.tabs[leaf.activeTabIdx];
		// If the active tab is a route, replace it in place — but only if no
		// other tab already holds this path (handled above). Carry the tab's
		// identity forward: this is a content swap of the same slot (global
		// nav to a new path), not a new tab, so it shouldn't read as one to
		// identity-keyed consumers (pane.tsx's PaneBody key).
		if (active && active.kind === 'route') {
			const replacement = carryTabUid(active, { kind: 'route', path } as PaneView);
			const tabs = leaf.tabs.map((t, i) => (i === leaf.activeTabIdx ? replacement : t));
			return { ...leaf, tabs };
		}
		const tabs: PaneView[] = [...leaf.tabs, { kind: 'route', path }];
		return { ...leaf, tabs, activeTabIdx: tabs.length - 1 };
	});
}
