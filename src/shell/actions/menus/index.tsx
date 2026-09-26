// D-06 Menus tab (WP-59): menu list · ordered tree editor · live preview.
// Replaces the wave 12e placeholder (item 19's mount contract) — this file
// is the one this folder exports as `MenusSurface`.
//
// Every write (reorder, hide/unhide, add action, add separator, remove
// separator, reset this menu) goes straight through G-ACTIONS-API
// (`@/lib/actions/store`) at the header's chosen `scope`; there is no local
// "unsaved" draft — the effective model re-merges on `actions://changed`
// (the write itself, or a concurrent on-disk edit) and this surface just
// re-renders from it, same as `actions-list.tsx` / `action-detail.tsx`.
//
// Review round 1 major 5 ("serialize writes ... build each write from
// getEffectiveMenu / this scope's override at commit time, never from stale
// rows"): a structural write (reorder, add action, add/remove separator)
// goes through `queueRowsWrite` below — a local promise chain (one write in
// flight at a time) that, when its turn comes, re-fetches the *live*
// effective model and rebuilds `rows` from *that*, then re-applies the
// intended change (a pure `MenuRow[] -> MenuRow[] | null` from
// `menu-model.ts`, keyed by row id/anchor, never a raw index) before
// writing. That's what keeps two rapid edits (a drag immediately followed
// by a hide) from one clobbering the other with a payload built from
// whatever `rows` this component had rendered several edits ago.
// `hideAction`/`unhideAction`/`resetMenuOverride` don't need this — they
// already read+merge fresh inside the store's own serialized edit queue
// (G-ACTIONS-API) — but are still routed through the same local queue so
// this surface never has more than one write outstanding at once, matching
// the review's "queue or pending guard".

import { useEffect, useMemo, useRef, useState } from 'react';
import { Info, Minus, Plus } from 'lucide-react';
import { useSearch } from '@tanstack/react-router';
import { Button } from '@/components/ui/button';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import {
	ActionsFileNotWritableError,
	ActionsValidationError,
	bindingsFor,
	getEffectiveModel,
	hideAction,
	LockedActionError,
	LowerScopeOverrideError,
	resetMenuOverride,
	setMenuOverride,
	unhideAction,
} from '@/lib/actions/store';
import { actionsPathLabel } from '../header';
import { menuLabel } from '../shared/menu-label';
import type { ActionsSurfaceProps } from '../types';
import { AddActionDialog } from './add-action-dialog';
import {
	appendActionRow,
	buildMenuRows,
	insertSeparator,
	type MenuRow,
	menuRowActionIds,
	moveRow,
	moveRowAfter,
	removeRowByKey,
	rowsToOverridePayload,
	separatorWouldCollapse,
} from './menu-model';
import { MenuList } from './menu-list';
import { MenuPreview, previewNote } from './menu-preview';
import { MenuTree } from './menu-tree';
import './menus.css';

function writeErrorMessage(err: unknown): string {
	if (
		err instanceof ActionsFileNotWritableError ||
		err instanceof ActionsValidationError ||
		err instanceof LowerScopeOverrideError ||
		err instanceof LockedActionError ||
		err instanceof Error
	) {
		return err.message;
	}
	return String(err);
}

export function MenusSurface({ scope, model, onNavigate }: ActionsSurfaceProps) {
	const search = useSearch({ strict: false }) as { action?: string };
	const menuIds = model.menus.ids;
	const [selectedMenuId, setSelectedMenuId] = useState<string | null>(menuIds.includes('files') ? 'files' : (menuIds[0] ?? null));
	const [writeError, setWriteError] = useState<string | null>(null);
	const [notice, setNotice] = useState<string | null>(null);
	const [addOpen, setAddOpen] = useState(false);
	const [highlightId, setHighlightId] = useState<string | null>(null);
	const [focusedIndex, setFocusedIndex] = useState<number | null>(null);
	const dispatchedForRef = useRef<string | null>(null);
	const writeQueueRef = useRef<Promise<void>>(Promise.resolve());

	// Keep a valid selection as the model reshapes (a package/project change
	// can add or remove `section/<id>` menus from `model.menus.ids`).
	useEffect(() => {
		if (selectedMenuId && menuIds.includes(selectedMenuId)) return;
		setSelectedMenuId(menuIds.includes('files') ? 'files' : (menuIds[0] ?? null));
	}, [menuIds, selectedMenuId]);

	useEffect(() => {
		setFocusedIndex(null);
		setNotice(null);
	}, [selectedMenuId]);

	useEffect(() => {
		if (!notice) return;
		const t = setTimeout(() => setNotice(null), 6000);
		return () => clearTimeout(t);
	}, [notice]);

	// Deep link (`?action=<id>`, the mount contract's `onNavigate` counterpart
	// from the Actions detail pane's "Show in menus"-style journeys): jump to
	// the first menu that carries this action, visible or hidden, and focus
	// its row once it's rendered.
	useEffect(() => {
		const wanted = search.action;
		if (!wanted || dispatchedForRef.current === wanted) return;
		for (const id of menuIds) {
			const menu = model.menus.get(id);
			if (!menu) continue;
			const present = menu.items.some((item) => item.kind === 'action' && item.id === wanted) || menu.hidden.includes(wanted);
			if (present) {
				dispatchedForRef.current = wanted;
				setSelectedMenuId(id);
				setHighlightId(wanted);
				break;
			}
		}
	}, [search.action, menuIds, model.menus]);

	const menu = selectedMenuId ? model.menus.get(selectedMenuId) : null;
	const rows = useMemo(() => buildMenuRows(menu, model.actionById, scope), [menu, model.actionById, scope]);

	const keyById = useMemo(() => {
		const map = new Map<string, string | null>();
		for (const row of rows) {
			if (row.kind !== 'action') continue;
			const bindings = bindingsFor(row.id);
			map.set(row.id, bindings.length > 0 ? bindings[0].key : null);
		}
		return map;
	}, [rows, model.keymap]);

	const addCandidates = useMemo(() => {
		const present = menuRowActionIds(rows);
		return model.actions.filter((a) => !present.has(a.id));
	}, [rows, model.actions]);

	/** Runs `task` strictly after every write already queued (Major 5's
	 *  "one write in flight at a time"). Errors are swallowed here — each
	 *  `task` reports its own via `setWriteError` — so the chain itself never
	 *  rejects and stalls every write after it. */
	function enqueue(task: () => Promise<void>) {
		writeQueueRef.current = writeQueueRef.current.then(task, task);
	}

	/** Structural writes (reorder, add/remove separator, add action):
	 *  `transform` runs against the *live* model at write time, not the rows
	 *  this render closed over (Major 5). `null` means "nothing to write"
	 *  (the target row/anchor is gone, or the change is no longer valid) —
	 *  a silent, safe no-op. */
	function queueRowsWrite(transform: (freshRows: MenuRow[]) => MenuRow[] | null) {
		const menuId = selectedMenuId;
		if (!menuId) return;
		enqueue(async () => {
			const freshModel = getEffectiveModel();
			const freshMenu = freshModel.menus.get(menuId);
			if (!freshMenu) return;
			const freshRows = buildMenuRows(freshMenu, freshModel.actionById, scope);
			const next = transform(freshRows);
			if (!next) return;
			setWriteError(null);
			try {
				await setMenuOverride(scope, menuId, rowsToOverridePayload(next, freshMenu.overrides[scope]));
			} catch (err) {
				setWriteError(writeErrorMessage(err));
			}
		});
	}

	/** `false` rejects the move outright (nothing queued) — `menu-tree.tsx`
	 *  only announces/refocuses when this returns `true`. */
	function onReorder(from: number, to: number): boolean {
		if (to < 0 || to >= rows.length || from === to || from < 0 || from >= rows.length) return false;
		const moved = rows[from];
		const preview = moveRow(rows, from, to);
		if (moved.kind === 'separator' && separatorWouldCollapse(preview, to)) {
			setNotice("A separator can't be first, last, or next to another separator.");
			return false;
		}
		const movedKey = moved.key;
		const afterKey = preview[to - 1]?.key ?? null;
		queueRowsWrite((freshRows) => moveRowAfter(freshRows, movedKey, afterKey));
		return true;
	}

	function onToggleHidden(row: Extract<MenuRow, { kind: 'action' }>) {
		if (!selectedMenuId) return;
		if (row.locked) {
			setNotice(`"${row.action.name}" is locked — it may move, but not be hidden.`);
			return;
		}
		if (row.hiddenElsewhere && !row.hiddenHere) {
			setNotice(`"${row.action.name}" is hidden at the ${row.hiddenElsewhere} scope — unhide it there.`);
			return;
		}
		const menuId = selectedMenuId;
		const wantHidden = !row.hiddenHere;
		const id = row.id;
		enqueue(async () => {
			setWriteError(null);
			try {
				if (wantHidden) await hideAction(scope, id, [menuId]);
				else await unhideAction(scope, id, [menuId]);
			} catch (err) {
				setWriteError(writeErrorMessage(err));
			}
		});
	}

	function onRemoveSeparator(index: number) {
		const key = rows[index]?.key;
		if (!key) return;
		queueRowsWrite((freshRows) => removeRowByKey(freshRows, key));
	}

	function onAddSeparator() {
		const afterKey = focusedIndex != null ? (rows[focusedIndex]?.key ?? null) : null;
		const preview = insertSeparator(rows, afterKey);
		if (!preview) {
			setNotice("Can't add a separator there — it would collapse (no leading, trailing or doubled separators).");
			return;
		}
		queueRowsWrite((freshRows) => insertSeparator(freshRows, afterKey));
	}

	function onAddAction(actionId: string) {
		const action = model.actionById.get(actionId);
		if (!action) return;
		queueRowsWrite((freshRows) => {
			const freshAction = getEffectiveModel().actionById.get(actionId) ?? action;
			return appendActionRow(freshRows, freshAction);
		});
		setAddOpen(false);
	}

	async function onResetMenu() {
		if (!selectedMenuId) return;
		const menuId = selectedMenuId;
		const label = menuLabel(menuId);
		const file = actionsPathLabel(scope, model.projectRoot);
		const ok = await confirmDialog(
			`This removes your customization of the "${label}" menu at ${scope} scope from ${file}. Its default order and visibility return.`,
			{ title: `Reset ${label}`, kind: 'warning', okLabel: 'Reset' }
		);
		if (!ok) return;
		enqueue(async () => {
			setWriteError(null);
			try {
				await resetMenuOverride(scope, menuId);
			} catch (err) {
				setWriteError(writeErrorMessage(err));
			}
		});
	}

	if (!selectedMenuId || !menu) {
		return (
			<div className="menus-surface" data-state="menus">
				<p className="cempty">No menus are known yet.</p>
			</div>
		);
	}

	return (
		<div className="menus-surface flex-1 min-h-0 flex flex-col" data-state="menus">
			{writeError && (
				<div className="issuesbanner" role="alert">
					<div className="issuesbanner-row">{writeError}</div>
				</div>
			)}
			{notice && !writeError && (
				<div className="issuesbanner" role="status">
					<div className="issuesbanner-row">{notice}</div>
				</div>
			)}
			<div className="menuwrap">
				<MenuList menuIds={menuIds} model={model} selected={selectedMenuId} onSelect={setSelectedMenuId} />
				<div className="treecol">
					<div className="treehead">
						<span className="t">{menuLabel(selectedMenuId)} menu</span>
						<span className="meta">{previewNote(rows)}</span>
						<span className="rt">
							<Button variant="outline" size="sm" className="min-h-[var(--btn-h-sm)] gap-1.5 text-xs" onClick={() => setAddOpen(true)}>
								<Plus className="h-3 w-3" />
								Add action…
							</Button>
							<Button variant="outline" size="sm" className="min-h-[var(--btn-h-sm)] gap-1.5 text-xs" onClick={onAddSeparator}>
								<Minus className="h-3 w-3" />
								Add separator
							</Button>
							<Button variant="outline" size="sm" className="min-h-[var(--btn-h-sm)] text-xs" onClick={() => void onResetMenu()}>
								Reset this menu
							</Button>
						</span>
					</div>
					<MenuTree
						rows={rows}
						keyById={keyById}
						onReorder={onReorder}
						onToggleHidden={onToggleHidden}
						onRemoveSeparator={onRemoveSeparator}
						highlightId={highlightId}
						onOpenEditor={(actionId) => onNavigate('editor', { action: actionId })}
						onRowFocus={setFocusedIndex}
					/>
					<div className="mfoot">
						<Info className="h-3 w-3" aria-hidden="true" />
						<span>
							Drag a handle to reorder, or focus a row and press <span style={{ fontFamily: 'var(--font-mono)' }}>⌥↑</span> /{' '}
							<span style={{ fontFamily: 'var(--font-mono)' }}>⌥↓</span> — or use the up/down buttons beside the handle. Locked items
							may move but not hide. Hiding never unbinds a key — it still fires either way.
						</span>
					</div>
				</div>
				<aside className="detailcol" style={{ width: 300, minWidth: 260, display: 'flex', flexDirection: 'column' }}>
					<div className="dhead">
						<div className="dtitle">
							<h2>Preview</h2>
							<span className="v">{previewNote(rows)}</span>
						</div>
						<div className="dsub">The real menu, as it will open.</div>
					</div>
					<MenuPreview rows={rows} keyById={keyById} />
				</aside>
			</div>
			<AddActionDialog open={addOpen} onOpenChange={setAddOpen} candidates={addCandidates} onPick={onAddAction} />
		</div>
	);
}
