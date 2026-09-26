// D-06 Keys tab + conflict state (WP-60). Keybinding table (Command · Key ·
// When · Source), search by command or by pressed keys, a macOS/Windows
// platform preview, rebinding through the shared key recorder, per-row Reset
// and Reset all, OS-wide rows (DEC-60) rebindable only in Personal, held
// project rows (DEC-65) linking to WP-53's trust sheet, and the DEC-57
// palette-delay warning. `data-state` on this surface's own root is dynamic
// (`keys` / `conflict`, G-ACTIONS §5) — whenever at least one hard clash is
// present for the previewed platform, this is the `conflict` state; shell.tsx's
// outer `.surface` wrapper still says `keys` (item 19 mount contract — it has
// no way to know about clashes), so both attributes are in the DOM at once.

import { useEffect, useMemo, useState } from 'react';
import { AlertTriangle, FileText, RotateCcw, Search, Shield, X } from 'lucide-react';
import { useSearch } from '@tanstack/react-router';
import { Button } from '@/components/ui/button';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import { openActionsFile } from '@/lib/actions/client';
import {
	ActionsFileNotWritableError,
	ActionsValidationError,
	addKeybinding,
	type EffectiveKeymapEntry,
	keyHolder,
	LowerScopeOverrideError,
	rebindKey,
	resetKeybindings,
	resetKeyOverride,
	unbindKey,
} from '@/lib/actions/store';
import { chordsByPrefix, chordPrefixDelays } from '@/lib/keymap/chord';
import { getOsShortcutStatuses, subscribeOsShortcutStatuses } from '@/lib/keymap/dispatcher';
import { isMacPlatform, resolveCombo, splitKeySequence, validateKeySequence } from '@/lib/keymap/platform';
import type { KeymapPlatform } from '@/lib/keymap/registry';
import { NgwaTrustSheet } from '@/shell/ngwa/ngwa-trust-sheet';
import { KeyRecorder } from '../shared/key-recorder';
import { Kbd } from '../shared/kbd';
import type { ActionsSurfaceProps } from '../types';
import {
	buildKeyRows,
	conflictsForRow,
	type KeyRow,
	matchesKey,
	matchesQuery,
	otherEntry,
	whenLabel,
} from './rows';

function actionsErrorMessage(err: unknown): string {
	if (
		err instanceof ActionsFileNotWritableError ||
		err instanceof ActionsValidationError ||
		err instanceof LowerScopeOverrideError ||
		err instanceof Error
	) {
		return err.message;
	}
	return String(err);
}

/** `KeymapEntry.source` spells the default layer `'default'`; the Actions
 *  tab's `.kind k-*` badges (reused here) spell it `'builtin'` — the same
 *  mapping `EffectiveAction.source` already uses for that layer. */
function sourceClass(source: string): string {
	return source === 'default' ? 'builtin' : source;
}

function sourceLabel(source: string): string {
	return source === 'default' ? 'builtin' : source;
}

function fileBaseName(root: string | null): string {
	return root?.replace(/[/\\]+$/, '').split(/[\\/]/).pop() ?? 'project';
}

function keybindingsPathLabel(scope: 'personal' | 'project', projectRoot: string | null): string {
	return scope === 'personal' ? '~/.ikenga/keybindings.json' : `${fileBaseName(projectRoot)}/.ikenga/keybindings.json`;
}

/** Live registration status for an OS-wide row (WP-54's `startOsShortcutSync`
 *  accessor, `keymap/dispatcher.ts`) — re-read on every publish. */
function useOsShortcutStatuses() {
	const [statuses, setStatuses] = useState(() => getOsShortcutStatuses());
	useEffect(() => subscribeOsShortcutStatuses(() => setStatuses(getOsShortcutStatuses())), []);
	return statuses;
}

export function KeysSurface({ scope, model }: ActionsSurfaceProps) {
	const search = useSearch({ strict: false }) as { action?: string } | undefined;
	const [previewPlatform, setPreviewPlatform] = useState<KeymapPlatform>(() => (isMacPlatform() ? 'mac' : 'other'));
	const [textQuery, setTextQuery] = useState('');
	const [keyQuery, setKeyQuery] = useState<string | null>(null);
	const [editingRowId, setEditingRowId] = useState<string | null>(null);
	const [restrictingRowId, setRestrictingRowId] = useState<string | null>(null);
	const [restrictValue, setRestrictValue] = useState('');
	const [rowError, setRowError] = useState<{ rowId: string; message: string } | null>(null);
	const [notice, setNotice] = useState<string | null>(null);
	const [trustOpen, setTrustOpen] = useState(false);
	const [confirmingResetAll, setConfirmingResetAll] = useState(false);

	const mac = previewPlatform === 'mac';
	const osStatuses = useOsShortcutStatuses();

	const rows = useMemo(() => buildKeyRows(model, previewPlatform), [model, previewPlatform]);
	const conflicts = model.keymap.conflicts[previewPlatform];

	// DEC-57: the rows the palette-delay warning marks — a single-stroke
	// binding that some bound chord makes a chord prefix (`chordPrefixDelays`,
	// `keymap/chord.ts` — built for exactly this row, WP-49 hand-off).
	const chordDelayCommands = useMemo(() => {
		const bound = rows.filter((r): r is KeyRow & { entry: EffectiveKeymapEntry } => r.kind === 'bound' && !!r.entry);
		return new Set(chordPrefixDelays(bound.map((r) => r.entry), mac).map((d) => d.binding.command));
	}, [rows, mac]);

	// Deep link from the Actions detail pane's Rebind… button (`?action=<id>`).
	useEffect(() => {
		if (!search?.action) return;
		const target = rows.find((r) => r.command === search.action && r.kind !== 'held');
		if (target) setEditingRowId(target.rowId);
	}, [search?.action, rows]);

	const filtered = useMemo(() => {
		if (keyQuery !== null) return rows.filter((r) => matchesKey(r, keyQuery, previewPlatform));
		return rows.filter((r) => matchesQuery(r, textQuery));
	}, [rows, textQuery, keyQuery, previewPlatform]);

	const boundCount = rows.filter((r) => r.kind === 'bound').length;
	const yoursCount = rows.filter((r) => r.kind === 'bound' && (r.source === 'personal' || r.source === 'project')).length;
	// `conflicts()` already yields each clashing pair once (`i < j` over the
	// same-key group), so its length is the pair count directly.
	const clashCount = conflicts.clashes.length;
	const hasClash = clashCount > 0;

	function clearRowError(rowId: string) {
		setRowError((e) => (e?.rowId === rowId ? null : e));
	}

	function osStatusFor(row: KeyRow): { registered: boolean; reason: string | null } | undefined {
		if (!row.osWide || !row.key) return undefined;
		return osStatuses.find((s) => s.command === row.command && s.key === row.key);
	}

	async function commitRecord(row: KeyRow, combo: string) {
		setRowError(null);
		try {
			if (row.kind === 'requested') {
				if (combo) await addKeybinding(scope, { key: combo, command: row.command });
				setEditingRowId(null);
				return;
			}
			const entry = row.entry;
			if (!entry) return;
			if (!combo) {
				await unbindKey(scope, entry);
				setEditingRowId(null);
				return;
			}
			// DEC-57: warn the moment this rebind creates the *first* chord for
			// this prefix — before it, that stroke resolved at once.
			if (validateKeySequence(combo) === null) {
				const strokes = splitKeySequence(combo);
				if (strokes.length === 2) {
					const firstResolved = resolveCombo(strokes[0], mac);
					const others = rows
						.filter((r): r is KeyRow & { entry: EffectiveKeymapEntry } => r.kind === 'bound' && !!r.entry && r.entry !== entry)
						.map((r) => r.entry);
					const already = (chordsByPrefix(others, mac).get(firstResolved)?.length ?? 0) > 0;
					if (!already) {
						setNotice(
							`${row.label} now starts with a chord: ${firstResolved.toUpperCase()} waits up to 900 ms for a second key — this delays every other command on that stroke, including the command palette.`
						);
					}
				}
			}
			await rebindKey(scope, entry, combo);
			setEditingRowId(null);
		} catch (err) {
			setRowError({ rowId: row.rowId, message: actionsErrorMessage(err) });
		}
	}

	async function handleReset(row: KeyRow) {
		setRowError(null);
		try {
			await resetKeyOverride(scope, row.command);
		} catch (err) {
			setRowError({ rowId: row.rowId, message: actionsErrorMessage(err) });
		}
	}

	async function handleUnbindOther(row: KeyRow, other: EffectiveKeymapEntry) {
		setRowError(null);
		try {
			await unbindKey(scope, other);
		} catch (err) {
			setRowError({ rowId: row.rowId, message: actionsErrorMessage(err) });
		}
	}

	async function submitRestrict(row: KeyRow) {
		const entry = row.entry;
		if (!entry || !restrictValue.trim()) return;
		setRowError(null);
		try {
			await rebindKey(scope, entry, entry.key, { when: restrictValue.trim() });
			setRestrictingRowId(null);
			setRestrictValue('');
		} catch (err) {
			setRowError({ rowId: row.rowId, message: actionsErrorMessage(err) });
		}
	}

	async function handleResetAll() {
		const file = keybindingsPathLabel(scope, model.projectRoot);
		const ok = await confirmDialog(`Reset every keybinding you've written at ${scope} scope? This rewrites ${file}.`, {
			title: 'Reset all keybindings',
			kind: 'warning',
			okLabel: 'Reset all',
		});
		if (!ok) return;
		setConfirmingResetAll(true);
		try {
			await resetKeybindings(scope);
		} catch (err) {
			setRowError({ rowId: '__all__', message: actionsErrorMessage(err) });
		} finally {
			setConfirmingResetAll(false);
		}
	}

	async function handleOpenFile() {
		try {
			await openActionsFile('keybindings', scope, scope === 'project' ? model.projectId : null);
		} catch (err) {
			setRowError({ rowId: '__all__', message: actionsErrorMessage(err) });
		}
	}

	function handleKeySearchRecord(combo: string) {
		if (!combo) {
			setKeyQuery(null);
			return;
		}
		setKeyQuery(combo);
		setTextQuery('');
		const holder = keyHolder(combo, previewPlatform);
		setNotice(holder ? `${combo.toUpperCase()} is taken` : `${combo.toUpperCase()} is free`);
	}

	useEffect(() => {
		if (!notice) return;
		const t = setTimeout(() => setNotice(null), 5000);
		return () => clearTimeout(t);
	}, [notice]);

	const dataState = hasClash ? 'conflict' : 'keys';

	return (
		<div data-state={dataState} className="keyswrap flex-1 min-h-0 flex flex-col">
			<div className="keyhead">
				<div className="search" style={{ minWidth: 230 }}>
					<Search className="h-3.5 w-3.5" />
					<input
						type="text"
						placeholder="Search by command…"
						aria-label="Search keybindings by command"
						value={textQuery}
						onChange={(e) => {
							setTextQuery(e.target.value);
							setKeyQuery(null);
						}}
					/>
					{(textQuery || keyQuery !== null) && (
						<button
							type="button"
							aria-label="Clear search"
							onClick={() => {
								setTextQuery('');
								setKeyQuery(null);
							}}
						>
							<X className="h-3 w-3" />
						</button>
					)}
				</div>
				<KeyRecorder
					size="sm"
					value={keyQuery}
					placeholder="Search by pressing keys"
					aria-label="Search keybindings by pressing keys"
					onRecord={handleKeySearchRecord}
				/>
				<span className="toolsep" />
				<span className="flabel">Platform</span>
				<span className="platseg" role="group" aria-label="Platform preview">
					<button type="button" className={mac ? 'on' : ''} onClick={() => setPreviewPlatform('mac')}>
						macOS
					</button>
					<button type="button" className={mac ? '' : 'on'} onClick={() => setPreviewPlatform('other')}>
						Windows / Linux
					</button>
				</span>
				<span className="rt" style={{ marginLeft: 'auto', display: 'flex', gap: 'var(--space-2)' }}>
					<Button variant="outline" size="sm" disabled={confirmingResetAll} onClick={() => void handleResetAll()}>
						<RotateCcw className="mr-1.5 h-3 w-3" />
						Reset all
					</Button>
					<Button variant="outline" size="sm" onClick={() => void handleOpenFile()}>
						<FileText className="mr-1.5 h-3 w-3" />
						Open keybindings.json
					</Button>
				</span>
			</div>

			{!mac && (
				<div className="keynotice" role="note">
					<span>
						On Windows/Linux, a <b>default</b> Ctrl+letter frame key (e.g. Ctrl+B) stays with the terminal while it has
						focus — only a <b>personal</b> rule on that key takes it from the PTY.
					</span>
				</div>
			)}
			{notice && (
				<div className="keynotice" role="status">
					<AlertTriangle className="h-3 w-3" />
					<span>{notice}</span>
				</div>
			)}
			{rowError?.rowId === '__all__' && (
				<div className="keynotice danger" role="alert">
					{rowError.message}
				</div>
			)}

			<div className="sc" style={{ flex: 1, minHeight: 0 }}>
				<table className="keys">
					<thead>
						<tr>
							<th>Command</th>
							<th className="c-key">Key</th>
							<th className="c-when">When</th>
							<th className="c-src">Source</th>
							<th className="c-act" />
						</tr>
					</thead>
					<tbody>
						{filtered.length === 0 && (
							<tr>
								<td colSpan={5}>
									<div className="empty">No binding matches.</div>
								</td>
							</tr>
						)}
						{filtered.map((row) => {
							const rc = conflictsForRow(row, conflicts);
							const hard = rc.clashes.length > 0;
							const editing = editingRowId === row.rowId;
							const restricting = restrictingRowId === row.rowId;
							const status = osStatusFor(row);
							const canEditOsWide = !row.osWide || scope === 'personal';
							const err = rowError?.rowId === row.rowId ? rowError.message : null;
							return (
								<KeyTableRow
									key={row.rowId}
									row={row}
									mac={mac}
									hard={hard}
									clashes={rc.clashes}
									precedence={rc.precedence}
									editing={editing}
									restricting={restricting}
									restrictValue={restrictValue}
									setRestrictValue={setRestrictValue}
									chordDelay={row.kind === 'bound' && chordDelayCommands.has(row.command) && !!row.key}
									osStatus={status}
									canEdit={canEditOsWide}
									error={err}
									onStartEdit={() => {
										setEditingRowId(row.rowId);
										clearRowError(row.rowId);
									}}
									onCancelEdit={() => setEditingRowId(null)}
									onRecord={(combo) => void commitRecord(row, combo)}
									onReset={() => void handleReset(row)}
									onUnbindOther={(other) => void handleUnbindOther(row, other)}
									onStartRestrict={() => {
										setRestrictingRowId(row.rowId);
										setRestrictValue(row.entry?.when ?? '');
									}}
									onCancelRestrict={() => setRestrictingRowId(null)}
									onSubmitRestrict={() => void submitRestrict(row)}
									onTrust={() => setTrustOpen(true)}
								/>
							);
						})}
					</tbody>
				</table>
			</div>

			<div className="mfoot">
				<FileText className="h-3.5 w-3.5" />
				<span>
					One registry — <span className="mono">{keybindingsPathLabel(scope, model.projectRoot)}</span> overrides the
					built-in map. {boundCount} bindings · {yoursCount} yours · {clashCount} conflict{clashCount === 1 ? '' : 's'}{' '}
					· chords such as <span className="mono">⌘K ⌘R</span> are supported.
				</span>
			</div>

			<NgwaTrustSheet
				open={trustOpen}
				onOpenChange={setTrustOpen}
				item={null}
				mode="project-actions"
				projectActions={{ projectId: model.projectId, projectName: fileBaseName(model.projectRoot) }}
			/>
		</div>
	);
}

interface KeyTableRowProps {
	row: KeyRow;
	mac: boolean;
	hard: boolean;
	clashes: ReturnType<typeof conflictsForRow>['clashes'];
	precedence: ReturnType<typeof conflictsForRow>['precedence'];
	editing: boolean;
	restricting: boolean;
	restrictValue: string;
	setRestrictValue: (v: string) => void;
	chordDelay: boolean;
	osStatus: { registered: boolean; reason: string | null } | undefined;
	canEdit: boolean;
	error: string | null;
	onStartEdit: () => void;
	onCancelEdit: () => void;
	onRecord: (combo: string) => void;
	onReset: () => void;
	onUnbindOther: (other: EffectiveKeymapEntry) => void;
	onStartRestrict: () => void;
	onCancelRestrict: () => void;
	onSubmitRestrict: () => void;
	onTrust: () => void;
}

function KeyTableRow({
	row,
	mac,
	hard,
	clashes,
	precedence,
	editing,
	restricting,
	restrictValue,
	setRestrictValue,
	chordDelay,
	osStatus,
	canEdit,
	error,
	onStartEdit,
	onCancelEdit,
	onRecord,
	onReset,
	onUnbindOther,
	onStartRestrict,
	onCancelRestrict,
	onSubmitRestrict,
	onTrust,
}: KeyTableRowProps) {
	const rowClass = editing ? 'editing' : hard ? 'clash' : row.kind === 'held' ? 'held' : '';
	const isOverride = row.kind === 'bound' && (row.source === 'personal' || row.source === 'project');
	// Bound once so every reference below (including inside the `onClick`
	// closures) narrows to `EffectiveKeymapEntry`, not `EffectiveKeymapEntry |
	// undefined` — TS does not keep `row.entry`'s narrowing across a closure.
	const clashEntry: EffectiveKeymapEntry | undefined = row.entry;
	return (
		<>
			<tr className={rowClass || undefined}>
				<td>
					<span className="cmdlbl">{row.label}</span>
					{row.command !== row.label && <span className="cmdid mono">{row.command}</span>}
					{row.kind === 'requested' && <span className="pchip">requested</span>}
					{row.kind === 'held' && <span className="pchip danger">held until trusted</span>}
					{isOverride && <span className="pchip">yours</span>}
				</td>
				<td className="c-key">
					{row.kind === 'requested' ? (
						editing ? (
							<KeyRecorder size="sm" onRecord={onRecord} onCancel={onCancelEdit} />
						) : (
							<span className="reqkey">
								<Kbd combo={null} mac={mac} empty="unbound" />
								{row.heldBy && (
									<span className="heldby">
										held by <b>{row.heldBy.kind === 'native-role' ? row.heldBy.key : row.heldBy.command}</b>
									</span>
								)}
							</span>
						)
					) : editing ? (
						<KeyRecorder size="sm" value={row.key} onRecord={onRecord} onCancel={onCancelEdit} />
					) : (
						<span className="reqkey">
							<Kbd combo={row.key} mac={mac} />
							{chordDelay && (
								<AlertTriangle
									className="h-3 w-3 chordwarn"
									aria-label="Starts a chord — the palette and every other command on this key now waits up to 900 ms"
								/>
							)}
						</span>
					)}
				</td>
				<td className="c-when mono">{whenLabel(row.when)}</td>
				<td className="c-src">
					<span className="srccell">
						<span className={`kind k-${sourceClass(row.source)}`}>
							<span className="src-dot" aria-hidden="true" />
							{sourceLabel(row.source)}
						</span>
						{row.osWide && <span className="pchip">OS-wide</span>}
						{osStatus && !osStatus.registered && (
							<span className="pchip danger" title={osStatus.reason ?? undefined}>
								not registered
							</span>
						)}
					</span>
				</td>
				<td className="c-act">
					{row.kind === 'held' ? (
						<button type="button" className="chip" onClick={onTrust}>
							<Shield className="h-3 w-3" /> Trust…
						</button>
					) : row.kind === 'requested' ? (
						!editing && (
							<button type="button" className="chip" onClick={onStartEdit}>
								Bind a key…
							</button>
						)
					) : (
						<>
							<button type="button" className="chip" disabled={!canEdit} onClick={editing ? onCancelEdit : onStartEdit}>
								{editing ? 'Cancel' : 'Edit'}
							</button>
							{isOverride && (
								<button type="button" className="chip" onClick={onReset}>
									Reset
								</button>
							)}
						</>
					)}
				</td>
			</tr>
			{error && (
				<tr className={rowClass || undefined}>
					<td colSpan={5}>
						<p className="rowerr" role="alert">
							{error}
						</p>
					</td>
				</tr>
			)}
			{!canEdit && row.osWide && (
				<tr>
					<td colSpan={5}>
						<p className="rowerr">OS-wide shortcuts are rebindable only in the Personal scope.</p>
					</td>
				</tr>
			)}
			{hard && clashEntry && (
				<tr className="clash">
					<td colSpan={5}>
						<div className="clashnote">
							<AlertTriangle className="h-3.5 w-3.5" />
							<span>
								Used by: <b>{otherEntry(clashes[0], clashEntry).command}</b> ({whenLabel(otherEntry(clashes[0], clashEntry).when)})
								{precedence.length > 0 && (
									<>
										{' '}
										· also <b>{otherEntry(precedence[0], clashEntry).command}</b> ({whenLabel(otherEntry(precedence[0], clashEntry).when)}) — that
										one is a documented precedence, not a clash
									</>
								)}
							</span>
							<span className="rt">
								<button type="button" className="chip" onClick={() => onUnbindOther(otherEntry(clashes[0], clashEntry))}>
									Unbind the other
								</button>
								{restricting ? (
									<>
										<input
											type="text"
											className="restrictinput mono"
											value={restrictValue}
											onChange={(e) => setRestrictValue(e.target.value)}
											placeholder="a narrower when, e.g. filesFocus"
											aria-label="Restrict this binding's when"
										/>
										<button type="button" className="chip" onClick={onSubmitRestrict}>
											Save
										</button>
										<button type="button" className="chip" onClick={onCancelRestrict}>
											Cancel
										</button>
									</>
								) : (
									<button type="button" className="chip" onClick={onStartRestrict}>
										Restrict to…
									</button>
								)}
								<button type="button" className="chip" onClick={onStartEdit}>
									Choose another key
								</button>
							</span>
						</div>
					</td>
				</tr>
			)}
		</>
	);
}
