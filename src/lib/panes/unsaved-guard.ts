// Unsaved-changes guard for closing tabs and panes (plans/file-editing
// Shape 1). Wraps the pane store's close actions for the UI entry points — the
// tab strip, the pane menu, the address bar and the ⌘W / pane-close commands.
// Agent and iyke closes (bridge, control listener) call the store directly and
// stay unguarded on purpose: no one is there to answer a prompt.
//
// A tab "holds unsaved edits" when the editing store has a dirty session for
// its (pane, file) — a mounted editor, or a draft stashed when the tab was
// switched away from. Confirming discards those sessions, so the editor's
// unmount does not stash the draft again.

import { confirm } from '@/lib/transport/dialog-shim';
import { dirtySessionKeys, useEditingStore } from '@/lib/editing/editing-store';
import { findLeaf } from './pane-reducer';
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
 * Resolves true when nothing is unsaved or the user agreed to discard; false
 * keeps everything open. A confirm that cannot be shown reads as "no".
 */
export async function confirmDiscard(paneId: string, views: PaneView[]): Promise<boolean> {
	const targets = artifactTargets(paneId, views);
	const keys = dirtySessionKeys(targets);
	if (keys.length === 0) return true;
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
	if (ok) useEditingStore.getState().discard(keys);
	return ok;
}

export async function guardedCloseTab(paneId: string, tabIdx: number): Promise<void> {
	const leaf = findLeaf(usePaneStore.getState().root, paneId);
	const view = leaf?.tabs[tabIdx];
	if (view && !(await confirmDiscard(paneId, [view]))) return;
	// Re-resolve: the tab strip may have changed while the prompt was open.
	const now = findLeaf(usePaneStore.getState().root, paneId);
	if (!now) return;
	const idx = view ? now.tabs.indexOf(view) : tabIdx;
	if (idx === -1) return;
	usePaneStore.getState().closeTab(paneId, idx);
}

export async function guardedCloseActiveTab(): Promise<void> {
	const { root, focusedId } = usePaneStore.getState();
	const leaf = findLeaf(root, focusedId);
	if (!leaf) return;
	await guardedCloseTab(leaf.id, leaf.activeTabIdx);
}

export async function guardedClosePane(paneId: string): Promise<void> {
	const leaf = findLeaf(usePaneStore.getState().root, paneId);
	if (leaf && !(await confirmDiscard(paneId, leaf.tabs))) return;
	usePaneStore.getState().closePane(paneId);
}

export async function guardedCloseFocusedPane(): Promise<void> {
	await guardedClosePane(usePaneStore.getState().focusedId);
}
