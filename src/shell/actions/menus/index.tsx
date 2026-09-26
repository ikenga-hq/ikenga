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

import { useEffect, useMemo, useRef, useState } from 'react';
import { Info, Minus, Plus } from 'lucide-react';
import { useSearch } from '@tanstack/react-router';
import { Button } from '@/components/ui/button';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import {
	ActionsFileNotWritableError,
	ActionsValidationError,
	bindingsFor,
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
import { buildMenuRows, type MenuRow, menuRowActionIds, moveRow, rowsToOverridePayload } from './menu-model';
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
	const [selectedMenuId, setSelectedMenuId] = useState<string | null>(menuIds[0] ?? null);
	const [writeError, setWriteError] = useState<string | null>(null);
	const [addOpen, setAddOpen] = useState(false);
	const [highlightId, setHighlightId] = useState<string | null>(null);
	const dispatchedForRef = useRef<string | null>(null);

	// Keep a valid selection as the model reshapes (a package/project change
	// can add or remove `section/<id>` menus from `model.menus.ids`).
	useEffect(() => {
		if (selectedMenuId && menuIds.includes(selectedMenuId)) return;
		setSelectedMenuId(menuIds[0] ?? null);
	}, [menuIds, selectedMenuId]);

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
	const rows = useMemo(() => buildMenuRows(menu, model.actionById), [menu, model.actionById]);

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

	async function commit(nextRows: readonly MenuRow[]) {
		if (!selectedMenuId) return;
		setWriteError(null);
		try {
			await setMenuOverride(scope, selectedMenuId, rowsToOverridePayload(nextRows));
		} catch (err) {
			setWriteError(writeErrorMessage(err));
		}
	}

	function onReorder(from: number, to: number) {
		const next = moveRow(rows, from, to);
		if (next !== rows) void commit(next);
	}

	async function onToggleHidden(row: Extract<MenuRow, { kind: 'action' }>) {
		if (!selectedMenuId) return;
		setWriteError(null);
		try {
			if (row.hidden) await unhideAction(scope, row.id, [selectedMenuId]);
			else await hideAction(scope, row.id, [selectedMenuId]);
		} catch (err) {
			setWriteError(writeErrorMessage(err));
		}
	}

	function onRemoveSeparator(index: number) {
		void commit(rows.filter((_, i) => i !== index));
	}

	function onAddSeparator() {
		void commit([...rows, { kind: 'separator', key: `sep-${rows.length}-${Date.now()}` }]);
	}

	function onAddAction(actionId: string) {
		const action = model.actionById.get(actionId);
		if (!action) return;
		void commit([...rows, { kind: 'action', key: actionId, id: actionId, action, hidden: false, locked: action.locked }]);
		setAddOpen(false);
	}

	async function onResetMenu() {
		if (!selectedMenuId) return;
		const label = menuLabel(selectedMenuId);
		const file = actionsPathLabel(scope, model.projectRoot);
		const ok = await confirmDialog(
			`This removes your customization of the "${label}" menu at ${scope} scope from ${file}. Its default order and visibility return.`,
			{ title: `Reset ${label}`, kind: 'warning', okLabel: 'Reset' }
		);
		if (!ok) return;
		setWriteError(null);
		try {
			await resetMenuOverride(scope, selectedMenuId);
		} catch (err) {
			setWriteError(writeErrorMessage(err));
		}
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
						onToggleHidden={(row) => void onToggleHidden(row)}
						onRemoveSeparator={onRemoveSeparator}
						highlightId={highlightId}
						onOpenEditor={(actionId) => onNavigate('editor', { action: actionId })}
					/>
					<div className="mfoot">
						<Info className="h-3 w-3" aria-hidden="true" />
						<span>
							Drag a handle to reorder, or focus a row and press <span style={{ fontFamily: 'var(--font-mono)' }}>⌥↑</span> /{' '}
							<span style={{ fontFamily: 'var(--font-mono)' }}>⌥↓</span> — or use the up/down buttons beside the handle. Locked items
							may move but not hide.
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
