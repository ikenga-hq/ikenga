// Unsaved-changes guard for closing tabs and panes (plans/file-editing
// Shape 1). Wraps the pane store's close actions for the UI entry points — the
// tab strip, the pane menu, the address bar and the ⌘W / pane-close commands.
// Agent and iyke closes (bridge, control listener) call the store directly and
// stay unguarded on purpose: no one is there to answer a prompt.
//
// A tab "holds unsaved edits" when the editing store has a dirty session for
// its (pane, file) — a mounted editor, or a draft stashed when the tab was
// switched away from. Confirming discards those sessions, so the editor's
// unmount does not stash the draft again — but only once the close has
// actually happened. The pane store refuses some closes (the last pane, a
// pinned tab); a session marked discarded for a close that never happened
// would hide live edits from this guard and from the reload prompt, and a
// later tab switch would drop them without asking. So a close that would be
// refused is not asked about at all, and the discard follows the close.

import { confirm } from '@/lib/transport/dialog-shim';
import { dirtySessionKeys, useEditingStore } from '@/lib/editing/editing-store';
import { closeLeaf, closeTab, findLeaf } from './pane-reducer';
import { usePaneStore } from './pane-store';
import type { PaneView } from './types';

type Target = { path: string; paneId: string | null };

function artifactTargets(paneId: string, views: PaneView[]): Target[] {
	const out: Target[] = [];
	for (const v of views) if (v.kind === 'artifact') out.push({ path: v.path, paneId });
	return out;
}

/**
 * Ask before discarding unsaved edits in `views` (tabs of pane `paneId`).
 * Resolves to the session keys to discard once the close succeeds (empty when
 * nothing is unsaved), or null when the user chose to keep editing. A confirm
 * that cannot be shown reads as "no". Discards nothing itself.
 */
export async function confirmDiscard(paneId: string, views: PaneView[]): Promise<string[] | null> {
	const targets = artifactTargets(paneId, views);
	const keys = dirtySessionKeys(targets);
	if (keys.length === 0) return [];
	const sessions = useEditingStore.getState().sessions;
	const names = keys
		.map((k) => (sessions[k]?.path ?? '').split(/[\\/]/).pop() ?? '')
		.filter(Boolean);
	const what = names.length === 1 ? `${names[0]} has` : `${names.length} files have`;
	const ok = await confirm(`${what} unsaved changes. Close and discard them?`, {
		title: 'Unsaved changes',
		kind: 'warning',
		okLabel: 'Discard',
		cancelLabel: 'Keep editing',
	});
	return ok ? keys : null;
}

/**
 * After a close: discard those of `keys` (sessions of pane `paneId`) whose
 * file no longer has a tab in that pane — the tabs that really closed. A tab
 * the store refused to close keeps its session, unsaved edits and all.
 */
export function discardClosedSessions(paneId: string, keys: string[]): void {
	if (keys.length === 0) return;
	const leaf = findLeaf(usePaneStore.getState().root, paneId);
	const open = new Set<string>();
	for (const t of leaf?.tabs ?? []) if (t.kind === 'artifact') open.add(t.path);
	const sessions = useEditingStore.getState().sessions;
	const closed = keys.filter((k) => {
		const s = sessions[k];
		return s !== undefined && !open.has(s.path);
	});
	if (closed.length > 0) useEditingStore.getState().discard(closed);
}

function canCloseTab(paneId: string, tabIdx: number): boolean {
	const { root, focusedId } = usePaneStore.getState();
	return closeTab(root, paneId, tabIdx, focusedId).ok;
}

function canClosePane(paneId: string): boolean {
	const { root, focusedId } = usePaneStore.getState();
	return closeLeaf(root, paneId, focusedId).ok;
}

export async function guardedCloseTab(paneId: string, tabIdx: number): Promise<void> {
	// The store would refuse this close (the only tab of the only pane, a
	// pinned tab): nothing to ask, nothing to discard.
	if (!canCloseTab(paneId, tabIdx)) return;
	const leaf = findLeaf(usePaneStore.getState().root, paneId);
	const view = leaf?.tabs[tabIdx];
	const keys = view ? await confirmDiscard(paneId, [view]) : [];
	if (keys === null) return;
	// Re-resolve: the tab strip may have changed while the prompt was open.
	const now = findLeaf(usePaneStore.getState().root, paneId);
	if (!now) return;
	const idx = view ? now.tabs.indexOf(view) : tabIdx;
	if (idx === -1) return;
	usePaneStore.getState().closeTab(paneId, idx);
	discardClosedSessions(paneId, keys);
}

export async function guardedCloseActiveTab(): Promise<void> {
	const { root, focusedId } = usePaneStore.getState();
	const leaf = findLeaf(root, focusedId);
	if (!leaf) return;
	await guardedCloseTab(leaf.id, leaf.activeTabIdx);
}

export async function guardedClosePane(paneId: string): Promise<void> {
	if (!canClosePane(paneId)) return;
	const leaf = findLeaf(usePaneStore.getState().root, paneId);
	const keys = leaf ? await confirmDiscard(paneId, leaf.tabs) : [];
	if (keys === null) return;
	usePaneStore.getState().closePane(paneId);
	discardClosedSessions(paneId, keys);
}

export async function guardedCloseFocusedPane(): Promise<void> {
	await guardedClosePane(usePaneStore.getState().focusedId);
}
