// Ngwa Scopes Matrix Surface (WP-16 / WP-16a / locked D-02 frame-workbench-v4.html).
//
// Rows are equipment keyed by (kind, name). Columns are Personal, then one
// column per registered project scope (active project first), then one column
// per *installed* engine. Every cell value is read from the snapshot; every
// control either calls a real command through the route's `actions` or is
// disabled with a stated reason (interaction spec §1.2). Destructive actions
// sit behind a confirm that names the exact path (DEC-30); a precedence
// conflict is read from `placements[].overridden_by` (DEC-31).

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { AlertTriangle, ArrowRight, Bot, Circle, CircleDot, Minus } from 'lucide-react';
import type { NgwaItem, NgwaKind, NgwaPlacement, NgwaScope } from '@ikenga/contract';
import type { ClaudeStoreKind, ClaudeStoreScope, EngineId } from '@/lib/tauri-cmd';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { kindIcon } from './ngwa-list';
import './ngwa.css';

// ─── Shared model (also used by the Health surface) ──────────────────────────

/** The engines D-02 draws a column for. */
export const ENGINE_IDS: readonly EngineId[] = ['claude', 'codex', 'gemini'];

/** Map an engine pkg's name (manifest id, e.g. `com.ikenga.engine-claude-code`)
 *  to the placement engine id it serves. `null` for an engine we draw no
 *  column for (opencode, pi, …). */
export function engineIdOfPkg(item: NgwaItem): EngineId | null {
	if (item.kind !== 'engine') return null;
	const n = item.name.toLowerCase();
	const m = n.match(/engine[-._/]?([a-z0-9-]+)$/);
	const slug = m ? m[1] : (n.split(/[./]/).pop() ?? n);
	if (slug === 'claude' || slug === 'claude-code') return 'claude';
	if (slug === 'codex' || slug === 'codex-cli') return 'codex';
	if (slug === 'gemini' || slug === 'gemini-cli') return 'gemini';
	return null;
}

/** THE engine-column signal (WP-16a must-fix 3). An engine is installed iff
 *  the snapshot holds an engine pkg for it. Scopes and Health both read this
 *  and nothing else, so they cannot disagree. */
export function installedEngines(items: readonly NgwaItem[]): Map<EngineId, NgwaItem> {
	const out = new Map<EngineId, NgwaItem>();
	for (const it of items) {
		const id = engineIdOfPkg(it);
		if (id && !out.has(id)) out.set(id, it);
	}
	return out;
}

const PKG_KINDS: ReadonlySet<string> = new Set(['app', 'engine', 'tool', 'sidecar']);

/** A kernel pkg (its id is the manifest id), as opposed to an Ọba / config-scan
 *  item whose id is `${kind}:${scope}:${name}` (gate §2). */
export function isPkgItem(it: NgwaItem): boolean {
	return PKG_KINDS.has(it.kind) && !it.id.startsWith(`${it.kind}:`);
}

export function scopeKeyOf(s: NgwaScope): string {
	return s.kind === 'personal' ? 'personal' : `project:${s.project_id}`;
}

/** Scope key → `ClaudeStoreScope` wire value. Personal is `'workspace'`. */
export function wireOf(key: string): ClaudeStoreScope {
	return key === 'personal' ? 'workspace' : (key as `project:${string}`);
}

/** The Ọba kind a primitive row writes through, or null for pkgs/schedules. */
export function storeKindOf(it: NgwaItem): ClaudeStoreKind | null {
	if (isPkgItem(it)) return null;
	switch (it.kind) {
		case 'skill':
		case 'agent':
		case 'command':
		case 'hook':
			return it.kind;
		case 'tool':
			return 'mcp';
		default:
			return null;
	}
}

function normPath(p: string): string {
	return p.replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase();
}

export type CellMark = 'on' | 'off' | 'link' | 'none' | 'conflict';

export interface MatrixRow {
	key: string;
	kind: NgwaKind;
	name: string;
	label: string;
	pkg: boolean;
	storeKind: ClaudeStoreKind | null;
	/** An Ọba store entry exists, so enable-into-scope has something to place. */
	storeBacked: boolean;
	items: NgwaItem[];
	byScope: Map<string, NgwaItem>;
}

export interface ScopeConflict {
	row: MatrixRow;
	personal: NgwaItem;
	/** The personal placement the scan says is shadowed. */
	shadowed: NgwaPlacement;
	/** `overridden_by` — the path of the shadowing placement. */
	shadowPath: string;
	project: NgwaItem | null;
	projectPlacement: NgwaPlacement | null;
}

export function buildRows(items: readonly NgwaItem[]): MatrixRow[] {
	const rows = new Map<string, MatrixRow>();
	for (const it of items) {
		const pkg = isPkgItem(it);
		const key =
			it.kind === 'schedule'
				? `schedule:${it.owner_pkg_id ?? ''}/${it.name}`
				: `${pkg ? 'pkg' : 'prim'}:${it.kind}:${it.name}`;
		let row = rows.get(key);
		if (!row) {
			row = {
				key,
				kind: it.kind,
				name: it.name,
				label:
					it.kind === 'schedule' && it.owner_pkg_id
						? `${it.owner_pkg_id} · ${it.display_name || it.name}`
						: it.display_name || it.name,
				pkg,
				storeKind: storeKindOf(it),
				storeBacked: false,
				items: [],
				byScope: new Map(),
			};
			rows.set(key, row);
		}
		row.items.push(it);
		const sk = scopeKeyOf(it.scope);
		if (!row.byScope.has(sk)) row.byScope.set(sk, it);
		if (!pkg && row.storeKind && it.scope.kind === 'personal' && it.install_path !== null) {
			row.storeBacked = true;
		}
	}
	return [...rows.values()].sort(
		(a, b) => a.label.localeCompare(b.label) || a.kind.localeCompare(b.kind)
	);
}

/** DEC-31: a conflict is a personal placement the scan marked `overridden_by`.
 *  Versions play no part (null-version skills are caught the same way). */
export function conflictOf(row: MatrixRow): ScopeConflict | null {
	if (row.pkg) return null;
	const personal = row.byScope.get('personal');
	if (!personal) return null;
	const shadowed = personal.placements.find(
		(p) => p.scope.kind === 'personal' && p.overridden_by !== null
	);
	if (!shadowed || shadowed.overridden_by === null) return null;
	const target = normPath(shadowed.overridden_by);
	for (const it of row.items) {
		if (it.scope.kind !== 'project') continue;
		const pp = it.placements.find((p) => normPath(p.path) === target);
		if (pp) {
			return { row, personal, shadowed, shadowPath: shadowed.overridden_by, project: it, projectPlacement: pp };
		}
	}
	return {
		row,
		personal,
		shadowed,
		shadowPath: shadowed.overridden_by,
		project: null,
		projectPlacement: null,
	};
}

function placementsIn(it: NgwaItem, scopeKey: string): NgwaPlacement[] {
	return it.placements.filter((p) => scopeKeyOf(p.scope) === scopeKey);
}

export function isLink(p: NgwaPlacement): boolean {
	return p.mechanism !== 'settings-key' && (p.link_target !== null || p.in_store);
}

export function scopeMark(row: MatrixRow, scopeKey: string, conflict: ScopeConflict | null): CellMark {
	const it = row.byScope.get(scopeKey);
	if (!it) return 'none';
	if (conflict && scopeKey === 'personal') return 'conflict';
	if (row.pkg || row.kind === 'schedule' || row.kind === 'workflow') {
		return it.state === 'enabled' ? 'on' : 'off';
	}
	const here = placementsIn(it, scopeKey).filter((p) => p.present);
	if (here.length === 0) return 'off';
	if (here.some(isLink)) return 'link';
	return 'on';
}

/** Engine cells read the union of the row's placements (must-fix 6). */
export function engineMark(row: MatrixRow, engine: string): CellMark {
	const ps = row.items.flatMap((it) => it.placements).filter((p) => p.engine === engine && p.present);
	if (ps.length === 0) return 'none';
	return ps.some(isLink) ? 'link' : 'on';
}

/** The Claude placement a scope-local Claude command acts on. */
export function claudePlacement(it: NgwaItem | undefined, scopeKey: string): NgwaPlacement | null {
	if (!it) return null;
	const ps = placementsIn(it, scopeKey).filter((p) => p.engine === 'claude');
	return ps.find((p) => p.present) ?? ps[0] ?? null;
}

// ─── Confirm dialog (DEC-30) — shared with Health ────────────────────────────

export interface ConfirmRequest {
	title: string;
	body: ReactNode;
	confirmLabel: string;
	run: () => Promise<unknown>;
}

export function NgwaConfirmDialog({
	request,
	onClose,
}: {
	request: ConfirmRequest | null;
	onClose: (result: { ok: true } | { ok: false; error: string } | null) => void;
}) {
	const [busy, setBusy] = useState(false);
	return (
		<Dialog
			open={request !== null}
			onOpenChange={(open) => {
				if (!open && !busy) onClose(null);
			}}
		>
			{request && (
				<DialogContent data-ngwa-confirm showCloseButton={false}>
					<DialogHeader>
						<DialogTitle>{request.title}</DialogTitle>
						<DialogDescription asChild>
							<div className="ngwa-confirm-body">{request.body}</div>
						</DialogDescription>
					</DialogHeader>
					<DialogFooter>
						<button type="button" className="chip" disabled={busy} onClick={() => onClose(null)}>
							Cancel
						</button>
						<button
							type="button"
							className="chip danger"
							disabled={busy}
							aria-busy={busy || undefined}
							onClick={async () => {
								setBusy(true);
								try {
									await request.run();
									onClose({ ok: true });
								} catch (e) {
									onClose({ ok: false, error: errText(e) });
								} finally {
									setBusy(false);
								}
							}}
						>
							{request.confirmLabel}
						</button>
					</DialogFooter>
				</DialogContent>
			)}
		</Dialog>
	);
}

export function errText(e: unknown): string {
	if (e instanceof Error) return e.message;
	if (typeof e === 'string') return e;
	try {
		return JSON.stringify(e);
	} catch {
		return String(e);
	}
}

/** One line per placement nature, as DEC-30 requires. */
export function PlacementNature({ p }: { p: NgwaPlacement }) {
	if (p.mechanism === 'settings-key') {
		return (
			<p>
				It is a settings entry inside <code>{p.path}</code>. The entry is removed from that file;
				the rest of the file is kept.
			</p>
		);
	}
	if (isLink(p)) {
		return (
			<p>
				It is a <b>symlink</b>
				{p.link_target ? (
					<>
						{' '}
						to <code>{p.link_target}</code>
					</>
				) : null}
				. Only the link is removed; the store copy survives.
			</p>
		);
	}
	return (
		<p>
			It is a <b>real file</b>, not a link. It is <b>deleted permanently</b>.
		</p>
	);
}

// ─── Surface ─────────────────────────────────────────────────────────────────

export interface ScopeColumn {
	key: string;
	label: string;
	sub: string;
	active: boolean;
}

/** Every mutating call the matrix can make. The route implements each with
 *  the real `tauri-cmd` command (via the claude-config mutation hooks) and
 *  invalidates the snapshot afterwards. */
export interface NgwaScopeActions {
	enable: (kind: ClaudeStoreKind, name: string, scope: ClaudeStoreScope) => Promise<unknown>;
	disable: (kind: ClaudeStoreKind, name: string, scope: ClaudeStoreScope) => Promise<unknown>;
	copy: (
		kind: ClaudeStoreKind,
		name: string,
		from: ClaudeStoreScope,
		to: ClaudeStoreScope
	) => Promise<unknown>;
	move: (
		kind: ClaudeStoreKind,
		name: string,
		from: ClaudeStoreScope,
		to: ClaudeStoreScope
	) => Promise<unknown>;
	remove: (kind: ClaudeStoreKind, name: string, scope: ClaudeStoreScope) => Promise<unknown>;
	enableFor: (
		engine: EngineId,
		kind: ClaudeStoreKind,
		name: string,
		scope: ClaudeStoreScope
	) => Promise<unknown>;
	disableFor: (
		engine: EngineId,
		kind: ClaudeStoreKind,
		name: string,
		scope: ClaudeStoreScope
	) => Promise<unknown>;
	pkgSetEnabled: (pkgId: string, enabled: boolean) => Promise<unknown>;
	pkgUninstall: (pkgId: string) => Promise<unknown>;
	openStore: () => void;
}

export interface NgwaScopesSurfaceProps {
	items: NgwaItem[];
	isLoading?: boolean;
	error?: Error | null;
	unreadableSources?: Array<{ source: string; error: string | null }>;
	scopes: ScopeColumn[];
	actions: NgwaScopeActions;
	kind?: string;
	onKindChange?: (kind: string) => void;
	search?: string;
	focusScope?: string;
}

const SCOPE_KINDS: readonly { id: string; label: string }[] = [
	{ id: '*', label: 'all' },
	{ id: 'app', label: 'app' },
	{ id: 'engine', label: 'engine' },
	{ id: 'tool', label: 'tool' },
	{ id: 'skill', label: 'skill' },
	{ id: 'agent', label: 'agent' },
	{ id: 'command', label: 'command' },
	{ id: 'hook', label: 'hook' },
	{ id: 'workflow', label: 'workflow' },
	{ id: 'schedule', label: 'schedule' },
];

const MARK_TITLE: Record<CellMark, string> = {
	on: 'enabled here',
	off: 'installed but disabled here',
	link: 'symlinked from the store',
	none: 'not present',
	conflict: 'shadowed by a nearer copy',
};

interface PopItem {
	label: string;
	sub?: string;
	danger?: boolean;
	disabledReason?: string;
	onSelect?: () => void;
	sep?: boolean;
}

interface OpenPop {
	id: string;
	title: string;
	items: PopItem[];
}

export function NgwaScopesSurface({
	items,
	isLoading = false,
	error = null,
	unreadableSources = [],
	scopes,
	actions,
	kind = '*',
	onKindChange,
	search,
	focusScope,
}: NgwaScopesSurfaceProps) {
	const [pop, setPop] = useState<OpenPop | null>(null);
	const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
	const [status, setStatus] = useState<{ tone: 'ok' | 'err'; text: string } | null>(null);
	const [pending, setPending] = useState(false);

	const engines = useMemo(() => installedEngines(items), [items]);
	const engineCols = ENGINE_IDS.filter((e) => engines.has(e));
	const hiddenEngines = ENGINE_IDS.filter((e) => !engines.has(e));

	const orderedScopes = useMemo(() => {
		const personal = scopes.filter((s) => s.key === 'personal');
		const projects = scopes.filter((s) => s.key !== 'personal');
		const rank = (s: ScopeColumn) => (s.key === focusScope ? 0 : s.active ? 1 : 2);
		projects.sort((a, b) => rank(a) - rank(b));
		return [...personal, ...projects];
	}, [scopes, focusScope]);
	const scopeLabel = useCallback(
		(key: string) => scopes.find((s) => s.key === key)?.label ?? key,
		[scopes]
	);

	const allRows = useMemo(() => buildRows(items), [items]);
	const searchedRows = useMemo(() => {
		const q = search?.trim().toLowerCase();
		if (!q) return allRows;
		return allRows.filter(
			(r) => r.name.toLowerCase().includes(q) || r.label.toLowerCase().includes(q)
		);
	}, [allRows, search]);
	const rows = useMemo(
		() => searchedRows.filter((r) => kind === '*' || r.kind === kind),
		[searchedRows, kind]
	);
	const conflicts = useMemo(() => {
		const m = new Map<string, ScopeConflict>();
		for (const r of allRows) {
			const c = conflictOf(r);
			if (c) m.set(r.key, c);
		}
		return m;
	}, [allRows]);

	const unregistered = useMemo(() => {
		const visible = new Set(orderedScopes.map((s) => s.key));
		const keys = new Set<string>();
		for (const it of items) {
			const k = scopeKeyOf(it.scope);
			if (!visible.has(k)) keys.add(k);
		}
		return [...keys];
	}, [items, orderedScopes]);

	const run = useCallback(async (label: string, fn: () => Promise<unknown>) => {
		setPending(true);
		setStatus(null);
		try {
			await fn();
			setStatus({ tone: 'ok', text: label });
		} catch (e) {
			setStatus({ tone: 'err', text: `${label} failed: ${errText(e)}` });
		} finally {
			setPending(false);
		}
	}, []);

	const closePop = useCallback(() => setPop(null), []);

	// ── Popover content ──
	function scopePopItems(row: MatrixRow, col: ScopeColumn): PopItem[] {
		const here = row.byScope.get(col.key);
		const mark = scopeMark(row, col.key, conflicts.get(row.key) ?? null);
		const scope = wireOf(col.key);
		const where = col.label;

		if (row.kind === 'schedule' || row.kind === 'workflow') {
			const why =
				row.kind === 'schedule'
					? `Schedules follow their pkg${row.items[0]?.owner_pkg_id ? ` ${row.items[0].owner_pkg_id}` : ''}; enable or disable the pkg instead`
					: 'Workflows have no producer yet (Phase 4)';
			return [
				{ label: 'Enable here', disabledReason: why },
				{ label: 'Move here', disabledReason: why },
				{ label: 'Copy here', disabledReason: why },
				{ sep: true, label: '' },
				{ label: 'Disable', disabledReason: why },
			];
		}

		if (row.pkg) {
			const one = 'A pkg lives in exactly one scope';
			if (!here) {
				const elsewhere = [...row.byScope.keys()].map(scopeLabel).join(', ');
				return [
					{ label: 'Enable here', disabledReason: `${one}; it is installed in ${elsewhere}` },
					{ label: 'Move here', disabledReason: one },
					{ label: 'Copy here', disabledReason: one },
					{ sep: true, label: '' },
					{ label: 'Disable', disabledReason: 'Not present here' },
					{ label: `Remove from ${where}`, danger: true, disabledReason: 'Not present here' },
				];
			}
			const builtin = here.origin.source === 'builtin';
			return [
				{
					label: 'Enable here',
					disabledReason: here.state === 'enabled' ? 'Already enabled here' : undefined,
					onSelect: () =>
						void run(`Enabled ${row.label}`, () => actions.pkgSetEnabled(here.id, true)),
				},
				{ label: 'Move here', disabledReason: `${one}; it is already here` },
				{ label: 'Copy here', disabledReason: `${one}; it is already here` },
				{ sep: true, label: '' },
				{
					label: 'Disable',
					disabledReason: here.state === 'enabled' ? undefined : 'Already disabled here',
					onSelect: () =>
						void run(`Disabled ${row.label}`, () => actions.pkgSetEnabled(here.id, false)),
				},
				{
					label: `Remove from ${where}`,
					danger: true,
					disabledReason: builtin ? 'Shipped with the shell and cannot be uninstalled; disable it instead' : undefined,
					onSelect: () =>
						setConfirm({
							title: `Uninstall ${row.label}`,
							confirmLabel: 'Uninstall',
							body: (
								<>
									<p>
										Uninstalls <code>{here.id}</code> from {where}: it is unregistered and its install
										record, settings and granted permissions are deleted.
									</p>
									<p>
										Its files at <code>{here.install_path ?? '—'}</code> are left on disk.
									</p>
									<p>There is no undo.</p>
								</>
							),
							run: () => actions.pkgUninstall(here.id),
						}),
				},
			];
		}

		const sk = row.storeKind;
		if (!sk) return [{ label: 'Enable here', disabledReason: 'This kind has no scope writer' }];
		const conflict = conflicts.get(row.key) ?? null;
		if (mark === 'conflict' && conflict) {
			return conflictPopItems(conflict);
		}
		const present = mark === 'on' || mark === 'link';
		const otherKeys = [...row.byScope.keys()].filter(
			(k) => k !== col.key && claudePlacement(row.byScope.get(k), k) !== null
		);
		const source = otherKeys.includes('personal') ? 'personal' : otherKeys[0];
		const cp = claudePlacement(here, col.key);
		const cpPresent = cp?.present ? cp : null;
		const sourceReason = !source ? 'Not placed for claude in any other scope' : undefined;
		const presentReason = here && placementsIn(here, col.key).length > 0 ? 'Already present here' : undefined;
		return [
			{
				label: 'Enable here',
				disabledReason: present
					? 'Already enabled here'
					: !row.storeBacked
						? 'Not in the Ọba store, so there is nothing to place; use Copy here'
						: undefined,
				onSelect: () =>
					void run(`Enabled ${row.label} in ${where}`, () => actions.enable(sk, row.name, scope)),
			},
			{
				label: 'Move here',
				sub: source ? `from ${scopeLabel(source)}` : undefined,
				disabledReason: presentReason ?? sourceReason,
				onSelect: () =>
					source &&
					void run(`Moved ${row.label} to ${where}`, () =>
						actions.move(sk, row.name, wireOf(source), scope)
					),
			},
			{
				label: 'Copy here',
				sub: source ? `from ${scopeLabel(source)}` : undefined,
				disabledReason: presentReason ?? sourceReason,
				onSelect: () =>
					source &&
					void run(`Copied ${row.label} into ${where}`, () =>
						actions.copy(sk, row.name, wireOf(source), scope)
					),
			},
			{ sep: true, label: '' },
			{
				label: 'Disable',
				disabledReason: !cpPresent
					? 'Not placed for claude here'
					: cpPresent.mechanism !== 'settings-key' && !isLink(cpPresent)
						? `A real file, not a store link; disabling would delete it. Use Remove from ${where}`
						: undefined,
				onSelect: () =>
					void run(`Disabled ${row.label} in ${where}`, () => actions.disable(sk, row.name, scope)),
			},
			{
				label: `Remove from ${where}`,
				danger: true,
				disabledReason: cp ? undefined : 'Nothing placed for claude here',
				onSelect: () => cp && setConfirm(removeRequest(row, sk, col.key, cp)),
			},
		];
	}

	function removeRequest(
		row: MatrixRow,
		sk: ClaudeStoreKind,
		scopeKey: string,
		p: NgwaPlacement
	): ConfirmRequest {
		const where = scopeLabel(scopeKey);
		return {
			title: `Remove ${row.label} from ${where}`,
			confirmLabel: 'Remove',
			body: (
				<>
					<p>
						This removes <code data-remove-path>{p.path}</code>.
					</p>
					<PlacementNature p={p} />
					<p>There is no undo.</p>
				</>
			),
			run: () => actions.remove(sk, row.name, wireOf(scopeKey)),
		};
	}

	function updatePersonalRequest(c: ScopeConflict): ConfirmRequest | null {
		const sk = c.row.storeKind;
		const personalClaude = claudePlacement(c.personal, 'personal');
		if (!sk || !c.project || !personalClaude) return null;
		const projKey = scopeKeyOf(c.project.scope);
		return {
			title: `Update personal ${c.row.label}`,
			confirmLabel: 'Overwrite personal',
			body: (
				<>
					<p>
						Copies the {scopeLabel(projKey)} version from{' '}
						<code>{c.projectPlacement?.path ?? c.shadowPath}</code> over{' '}
						<code data-overwrite-path>{personalClaude.path}</code>.
					</p>
					{isLink(personalClaude) ? (
						<p>
							The personal copy is a <b>symlink</b>: the link is replaced by a real copy and the store
							copy survives.
						</p>
					) : (
						<p>
							The personal copy is a <b>real file</b>: its current contents are overwritten.
						</p>
					)}
					<p>There is no undo.</p>
				</>
			),
			run: () => actions.copy(sk, c.row.name, wireOf(projKey), 'workspace'),
		};
	}

	function conflictPopItems(c: ScopeConflict): PopItem[] {
		const upd = updatePersonalRequest(c);
		const sk = c.row.storeKind;
		const personalClaude = claudePlacement(c.personal, 'personal');
		return [
			{
				label: 'Update personal',
				disabledReason: upd
					? undefined
					: !c.project
						? `The shadowing copy (${c.shadowPath}) is not in a registered project`
						: 'No claude placement in personal to overwrite',
				onSelect: () => upd && setConfirm(upd),
			},
			{
				label: 'Remove from Personal',
				danger: true,
				disabledReason: personalClaude && sk ? undefined : 'Nothing placed for claude in personal',
				onSelect: () =>
					personalClaude && sk && setConfirm(removeRequest(c.row, sk, 'personal', personalClaude)),
			},
		];
	}

	function enginePopItems(row: MatrixRow, engine: EngineId): PopItem[] {
		const sk = row.storeKind;
		if (row.pkg || !sk) {
			const why = row.pkg
				? 'A pkg is not placed per engine'
				: row.kind === 'schedule'
					? 'Schedules are not placed per engine'
					: 'This kind has no engine writer';
			return [
				{ label: 'Enable here', disabledReason: why },
				{ label: 'Move here', disabledReason: why },
				{ label: 'Copy here', disabledReason: why },
				{ sep: true, label: '' },
				{ label: 'Disable', disabledReason: why },
			];
		}
		const mark = engineMark(row, engine);
		const cross = 'Cross-engine move and copy are not wired on this screen';
		const placed = row.items
			.flatMap((it) => it.placements)
			.find((p) => p.engine === engine && p.present);
		return [
			{
				label: 'Enable here',
				sub: 'in Personal',
				disabledReason:
					mark !== 'none'
						? `Already placed for ${engine}`
						: !row.storeBacked
							? 'Not in the Ọba store, so there is nothing to place'
							: undefined,
				onSelect: () =>
					void run(`Enabled ${row.label} for ${engine}`, () =>
						actions.enableFor(engine, sk, row.name, 'workspace')
					),
			},
			{ label: 'Move here', disabledReason: cross },
			{ label: 'Copy here', disabledReason: cross },
			{ sep: true, label: '' },
			{
				label: 'Disable',
				sub: placed ? `in ${scopeLabel(scopeKeyOf(placed.scope))}` : undefined,
				disabledReason: !placed
					? `Not placed for ${engine}`
					: placed.mechanism !== 'settings-key' && !isLink(placed)
						? 'A real file, not a store link; disabling would delete it'
						: undefined,
				onSelect: () =>
					placed &&
					void run(`Disabled ${row.label} for ${engine}`, () =>
						actions.disableFor(engine, sk, row.name, wireOf(scopeKeyOf(placed.scope)))
					),
			},
		];
	}

	// ── Enable all (D-02: confirm, then apply) ──
	function enableAllScope(col: ScopeColumn) {
		const scope = wireOf(col.key);
		const targets: Array<{ label: string; go: () => Promise<unknown> }> = [];
		for (const row of rows) {
			const mark = scopeMark(row, col.key, conflicts.get(row.key) ?? null);
			if (mark !== 'none' && mark !== 'off') continue;
			const here = row.byScope.get(col.key);
			if (row.pkg) {
				if (here && here.state !== 'enabled') {
					targets.push({ label: row.label, go: () => actions.pkgSetEnabled(here.id, true) });
				}
			} else if (row.storeKind && row.storeBacked) {
				const sk = row.storeKind;
				targets.push({ label: row.label, go: () => actions.enable(sk, row.name, scope) });
			}
		}
		return targets;
	}
	function enableAllEngine(engine: EngineId) {
		const targets: Array<{ label: string; go: () => Promise<unknown> }> = [];
		for (const row of rows) {
			if (row.pkg || !row.storeKind || !row.storeBacked) continue;
			if (engineMark(row, engine) !== 'none') continue;
			const sk = row.storeKind;
			targets.push({
				label: row.label,
				go: () => actions.enableFor(engine, sk, row.name, 'workspace'),
			});
		}
		return targets;
	}
	function askEnableAll(where: string, targets: Array<{ label: string; go: () => Promise<unknown> }>) {
		setConfirm({
			title: `Enable all in ${where}`,
			confirmLabel: `Enable ${targets.length}`,
			body: (
				<>
					<p>
						This enables <b>{targets.length}</b> item{targets.length === 1 ? '' : 's'} in <b>{where}</b>.
						Items already enabled there are untouched.
					</p>
					<p>{targets.map((t) => t.label).join(', ')}</p>
				</>
			),
			run: async () => {
				const failed: string[] = [];
				for (const t of targets) {
					try {
						await t.go();
					} catch (e) {
						failed.push(`${t.label}: ${errText(e)}`);
					}
				}
				if (failed.length) {
					throw new Error(
						`${targets.length - failed.length} of ${targets.length} enabled; failed — ${failed.join('; ')}`
					);
				}
			},
		});
	}

	// ── Render ──
	if (isLoading) {
		return (
			<div className="view-ngwa flex-1 min-h-0 flex items-center justify-center">
				<span className="text-sm text-muted-foreground" data-snapshot-loading>
					Reading the snapshot… the first rescan can take 1–2 minutes.
				</span>
			</div>
		);
	}

	if (error) {
		return (
			<div className="view-ngwa flex-1 min-h-0 p-4">
				<div className="source-banner" role="alert">
					<AlertTriangle className="h-4 w-4" />
					<span>Failed to load scopes: {error.message}</span>
				</div>
			</div>
		);
	}

	const conflictList = [...conflicts.values()];
	const conflictSourcesDown = unreadableSources.filter(
		(s) => s.source === 'engine_config' || s.source === 'oba'
	);
	const kindCounts = new Map<string, number>();
	for (const r of searchedRows) kindCounts.set(r.kind, (kindCounts.get(r.kind) ?? 0) + 1);

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			{unreadableSources.length > 0 && (
				<div className="source-banner" role="alert" data-unreadable>
					<AlertTriangle className="h-4 w-4" />
					<span>
						Unreadable: {unreadableSources.map((s) => `${s.source}${s.error ? ` (${s.error})` : ''}`).join('; ')}
						. Rows from those sources are missing, not absent.
					</span>
				</div>
			)}
			{/* ── Facet Bar ── */}
			<div className="facetbar">
				<div className="frow2">
					<span className="flabel">Kind</span>
					{SCOPE_KINDS.map((k) => {
						const cnt = k.id === '*' ? searchedRows.length : (kindCounts.get(k.id) ?? 0);
						const on = kind === k.id;
						return (
							<button
								key={k.id}
								type="button"
								data-mk={k.id}
								className={`chip ${on ? 'on' : ''}`}
								aria-pressed={on}
								onClick={() => onKindChange?.(k.id)}
							>
								{k.label} <span className="n">{cnt}</span>
							</button>
						);
					})}
					<span className="meta ml-auto" data-mcount>
						{rows.length} items × {orderedScopes.length} scopes × {engineCols.length} engines
					</span>
				</div>
			</div>

			<div className="mwrap">
				<div className="mcol">
					<div className="mscroll sc">
						<table className="matrix" aria-label="Scope and Engine Matrix">
							<thead>
								<tr>
									<th className="left">Equipment</th>
									{orderedScopes.map((col) => {
										const targets = enableAllScope(col);
										return (
											<th
												key={col.key}
												className={col.active || col.key === focusScope ? 'active' : undefined}
												data-col={col.key}
											>
												<span className="colname">{col.label}</span>
												<span className="colsub">{col.sub}</span>
												<button
													type="button"
													className="chip enall"
													data-enall={col.key}
													disabled={pending || targets.length === 0}
													title={
														targets.length === 0
															? `Every eligible row is already enabled in ${col.label}`
															: undefined
													}
													onClick={() => askEnableAll(col.label, targets)}
												>
													Enable all
												</button>
											</th>
										);
									})}
									{engineCols.map((eng) => {
										const targets = enableAllEngine(eng);
										return (
											<th key={eng} className="eng" data-col={eng}>
												<span className="colname">{eng}</span>
												<span className="colsub">{engines.get(eng)?.version ?? '—'}</span>
												<button
													type="button"
													className="chip enall"
													data-enall={eng}
													disabled={pending || targets.length === 0}
													title={
														targets.length === 0
															? `Every eligible row is already placed for ${eng}`
															: undefined
													}
													onClick={() => askEnableAll(eng, targets)}
												>
													Enable all
												</button>
											</th>
										);
									})}
								</tr>
							</thead>
							<tbody data-mbody>
								{rows.map((row) => {
									const conflict = conflicts.get(row.key) ?? null;
									return (
										<tr key={row.key} data-row={row.key}>
											<td className="item">
												<div className="in">
													{kindIcon(row.kind)}
													<span className="font-medium text-xs">{row.label}</span>
													<span className={`kind k-${row.kind}`}>{row.kind}</span>
												</div>
											</td>
											{orderedScopes.map((col) => {
												const id = `${row.key}|${col.key}`;
												const mark = scopeMark(row, col.key, conflict);
												const v = row.byScope.get(col.key)?.version;
												return (
													<td key={col.key} className="cell">
														<MatrixCell
															id={id}
															mark={mark}
															version={mark === 'conflict' ? (v ?? '—') : undefined}
															label={`${row.label} in ${col.label}: ${MARK_TITLE[mark]}`}
															open={pop?.id === id}
															onOpen={() =>
																setPop(
																	pop?.id === id
																		? null
																		: { id, title: `${col.label} · ${row.label}`, items: scopePopItems(row, col) }
																)
															}
															pop={pop?.id === id ? pop : null}
															onClosePop={closePop}
														/>
													</td>
												);
											})}
											{engineCols.map((eng) => {
												const id = `${row.key}|${eng}`;
												const mark = engineMark(row, eng);
												return (
													<td key={eng} className="cell eng">
														<MatrixCell
															id={id}
															mark={mark}
															label={`${row.label} for ${eng}: ${MARK_TITLE[mark]}`}
															open={pop?.id === id}
															onOpen={() =>
																setPop(
																	pop?.id === id
																		? null
																		: { id, title: `${eng} · ${row.label}`, items: enginePopItems(row, eng) }
																)
															}
															pop={pop?.id === id ? pop : null}
															onClosePop={closePop}
														/>
													</td>
												);
											})}
										</tr>
									);
								})}
							</tbody>
						</table>
					</div>

					{status && (
						<div className={`mstatus ${status.tone}`} role="status" data-mstatus>
							{status.text}
						</div>
					)}
					{hiddenEngines.map((eng) => (
						<div className="mfoot" key={eng} data-enginefoot={eng}>
							<Bot className="h-3.5 w-3.5" />
							<span>
								<strong>{eng}</strong> is not installed, so its column is not shown.
							</span>
							<button
								type="button"
								className="chip ml-auto"
								data-act="installengine"
								onClick={actions.openStore}
								title="Opens the Store; engines are listed there"
							>
								Open Store
							</button>
						</div>
					))}
					{unregistered.length > 0 && (
						<div className="mfoot" data-unregistered>
							<AlertTriangle className="h-3.5 w-3.5" />
							<span>
								Some items sit in scopes that are not registered projects and have no column:{' '}
								{unregistered.join(', ')}
							</span>
						</div>
					)}
				</div>

				{/* ── Sidenote ── */}
				<aside className="sidenote sc" data-sidenote aria-label="Matrix precedence and legend">
					<div className="subhead first">Precedence</div>
					{conflictSourcesDown.length > 0 ? (
						<div className="snote warn" data-conflicts-unknown>
							Conflicts unknown: {conflictSourcesDown.map((s) => s.source).join(', ')} unreadable.
						</div>
					) : conflictList.length > 0 ? (
						conflictList.map((c) => {
							const projKey = c.project ? scopeKeyOf(c.project.scope) : null;
							const upd = updatePersonalRequest(c);
							const personalClaude = claudePlacement(c.personal, 'personal');
							return (
								<div key={c.row.key} className="conflictbox" data-conflict={c.row.key}>
									<AlertTriangle className="h-4 w-4 flex-none" />
									<div className="txt">
										<span className="t1">{c.row.label} exists twice</span>
										<span className="t2">
											personal <code>{c.personal.version ?? '—'}</code> ·{' '}
											{projKey ? scopeLabel(projKey) : c.shadowPath}{' '}
											<code>{c.project?.version ?? '—'}</code>. The nearer scope wins, so inside{' '}
											{projKey ? scopeLabel(projKey) : 'that project'} every session loads the project
											copy and the personal copy is shadowed.
										</span>
										<div className="acts">
											<button
												type="button"
												className="chip"
												data-act="update-personal"
												disabled={!upd}
												title={upd ? undefined : 'The shadowing copy is not in a registered project'}
												onClick={() => upd && setConfirm(upd)}
											>
												Update personal
											</button>
											<button
												type="button"
												className="chip clear"
												data-act="remove-personal"
												disabled={!personalClaude || !c.row.storeKind}
												title={personalClaude ? undefined : 'Nothing placed for claude in personal'}
												onClick={() =>
													personalClaude &&
													c.row.storeKind &&
													setConfirm(removeRequest(c.row, c.row.storeKind, 'personal', personalClaude))
												}
											>
												Remove personal
											</button>
										</div>
									</div>
								</div>
							);
						})
					) : (
						<div className="snote" data-no-conflicts>
							No precedence conflicts: no personal item is shadowed by a project copy.
						</div>
					)}

					<div className="subhead">Reading a cell</div>
					<div className="legend">
						<div>
							<CircleDot className="h-3.5 w-3.5 mk-on" />
							<span>enabled in this scope</span>
						</div>
						<div>
							<Circle className="h-3.5 w-3.5" />
							<span>installed but disabled</span>
						</div>
						<div>
							<ArrowRight className="h-3.5 w-3.5 mk-link" />
							<span>symlinked from the store, not copied</span>
						</div>
						<div>
							<Minus className="h-3.5 w-3.5" />
							<span>not present here</span>
						</div>
						<div>
							<AlertTriangle className="h-3.5 w-3.5 mk-conflict" />
							<span>shadowed by a nearer copy of the same name</span>
						</div>
					</div>
					<p className="note">
						Click any cell to enable, move or copy the item into that scope or engine. Column headers
						act on the whole column.
					</p>
				</aside>
			</div>

			<NgwaConfirmDialog
				request={confirm}
				onClose={(result) => {
					const title = confirm?.title ?? '';
					setConfirm(null);
					setPop(null);
					if (result?.ok) setStatus({ tone: 'ok', text: `${title}: done` });
					else if (result && !result.ok) setStatus({ tone: 'err', text: `${title} failed: ${result.error}` });
				}}
			/>
		</div>
	);
}

function MarkIcon({ mark }: { mark: CellMark }) {
	switch (mark) {
		case 'on':
			return <CircleDot className="h-3.5 w-3.5" />;
		case 'off':
			return <Circle className="h-3.5 w-3.5" />;
		case 'link':
			return <ArrowRight className="h-3.5 w-3.5" />;
		case 'conflict':
			return <AlertTriangle className="h-3.5 w-3.5" />;
		default:
			return <Minus className="h-3 w-3" />;
	}
}

function MatrixCell({
	id,
	mark,
	version,
	label,
	open,
	onOpen,
	pop,
	onClosePop,
}: {
	id: string;
	mark: CellMark;
	version?: string;
	label: string;
	open: boolean;
	onOpen: () => void;
	pop: OpenPop | null;
	onClosePop: () => void;
}) {
	const btnRef = useRef<HTMLButtonElement>(null);
	return (
		<div className="cellwrap">
			<button
				ref={btnRef}
				type="button"
				className={`cellbtn ${mark === 'none' ? 'no' : mark} ${open ? 'open' : ''}`}
				data-cell={id}
				data-mark={mark}
				aria-haspopup="menu"
				aria-expanded={open}
				aria-label={label}
				title={label}
				onClick={onOpen}
			>
				<MarkIcon mark={mark} />
				{version !== undefined && <span className="cellver">{version}</span>}
			</button>
			{pop && <CellPopover pop={pop} anchor={btnRef} onClose={onClosePop} />}
		</div>
	);
}

function CellPopover({
	pop,
	anchor,
	onClose,
}: {
	pop: OpenPop;
	anchor: React.RefObject<HTMLButtonElement | null>;
	onClose: () => void;
}) {
	const ref = useRef<HTMLDivElement>(null);
	useEffect(() => {
		const first = ref.current?.querySelector<HTMLButtonElement>('button:not([disabled])');
		first?.focus();
		function onKey(e: KeyboardEvent) {
			if (e.key === 'Escape') {
				e.preventDefault();
				onClose();
				anchor.current?.focus();
			}
		}
		function onDown(e: MouseEvent) {
			const t = e.target as Node;
			if (ref.current?.contains(t) || anchor.current?.contains(t)) return;
			onClose();
		}
		document.addEventListener('keydown', onKey);
		document.addEventListener('mousedown', onDown);
		return () => {
			document.removeEventListener('keydown', onKey);
			document.removeEventListener('mousedown', onDown);
		};
	}, [onClose, anchor]);
	return (
		<div ref={ref} className="cellpop" role="menu" aria-label={pop.title} data-cellpop>
			<div className="mgroup">{pop.title}</div>
			{pop.items.map((it, i) =>
				it.sep ? (
					<div key={`sep-${i}`} className="msep" role="separator" />
				) : (
					<button
						key={it.label}
						type="button"
						role="menuitem"
						className={`mitem ${it.danger ? 'danger' : ''}`}
						disabled={it.disabledReason !== undefined}
						title={it.disabledReason}
						onClick={() => {
							if (it.disabledReason !== undefined) return;
							it.onSelect?.();
							onClose();
						}}
					>
						<span>{it.label}</span>
						{it.sub && <span className="msub"> {it.sub}</span>}
					</button>
				)
			)}
		</div>
	);
}
