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
import { AlertTriangle, ArrowRight, Bot, Circle, CircleDot, HelpCircle, Minus } from 'lucide-react';
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

export type CellMark = 'on' | 'off' | 'link' | 'none' | 'conflict' | 'unknown';

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
			return {
				row,
				personal,
				shadowed,
				shadowPath: shadowed.overridden_by,
				project: it,
				projectPlacement: pp,
			};
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

/** A symlink on disk. Only `link_target` counts: every symlink has one,
 *  dangling ones included. `mechanism` is the layout's *intended* mechanism
 *  (the golden snapshot has `symlink-dir` real folders), and `in_store` is
 *  computed by canonicalizing the whole path, so a REAL folder under a
 *  symlinked `~/.claude/skills` reads `in_store: true` — trusting it would
 *  call the store's own copy "a link" and let Remove delete it. */
export function isLink(p: NgwaPlacement): boolean {
	return p.mechanism !== 'settings-key' && p.link_target !== null;
}

export function scopeMark(
	row: MatrixRow,
	scopeKey: string,
	conflict: ScopeConflict | null,
	unknown = false
): CellMark {
	if (unknown && !row.pkg) return 'unknown';
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
export function engineMark(row: MatrixRow, engine: string, unknown = false): CellMark {
	if (unknown && !row.pkg) return 'unknown';
	const ps = row.items
		.flatMap((it) => it.placements)
		.filter((p) => p.engine === engine && p.present);
	if (ps.length === 0) return 'none';
	return ps.some(isLink) ? 'link' : 'on';
}

/** The Claude placement a scope-local Claude command acts on. */
export function claudePlacement(it: NgwaItem | undefined, scopeKey: string): NgwaPlacement | null {
	if (!it) return null;
	const ps = placementsIn(it, scopeKey).filter((p) => p.engine === 'claude');
	return ps.find((p) => p.present) ?? ps[0] ?? null;
}

// ─── Target paths: what the backend will actually touch ──────────────────────

function joinPath(root: string, ...parts: string[]): string {
	return [root.replace(/[\\/]+$/, ''), ...parts].join('/');
}

/** Folder primitives (skills) are removed, replaced and placed as a whole
 *  folder (`remove_dir_all` / `atomic_copy_dir`), even though the scan's
 *  placement path is the `SKILL.md` inside it. */
export function isDirKind(sk: ClaudeStoreKind | null): boolean {
	return sk === 'skill';
}

/** The node on disk a placement stands for: the folder for a skill. */
export function placementTarget(p: NgwaPlacement, sk: ClaudeStoreKind | null): string {
	return isDirKind(sk) ? p.path.replace(/[\\/]SKILL\.md$/i, '') : p.path;
}

/** The path the Rust command writes or deletes for (engine, kind, name) under
 *  `root`. Mirrors `scope_path_for` (claude) and `engine_file_path` (skills on
 *  gemini/codex, via `.agents/skills/`). `null` when it cannot be known here:
 *  no root, a settings kind, or a user-tier engine file whose extension the
 *  layout decides. */
export function expectedPath(
	engine: string,
	sk: ClaudeStoreKind | null,
	name: string,
	root: string | null
): string | null {
	if (!root || !sk || sk === 'hook' || sk === 'mcp' || sk === 'bundle') return null;
	if (engine === 'claude') {
		return sk === 'skill'
			? joinPath(root, '.claude', 'skills', name)
			: joinPath(root, '.claude', `${sk}s`, `${name}.md`);
	}
	if ((engine === 'gemini' || engine === 'codex') && sk === 'skill') {
		return joinPath(root, '.agents', 'skills', name);
	}
	return null;
}

export interface ResolvedRoot {
	root: string | null;
	/** Why the root cannot be used, when `root` is null. */
	why?: string;
}

const ROOT_UNKNOWN = 'Scope root unknown, so the target path cannot be checked';

/** Resolve a scope root the way the backend does, or refuse. A leading `~` is
 *  expanded with the resolved home; anything with a variable (`$`, `%`), a
 *  `~user` form or a relative path cannot be resolved here exactly as Rust
 *  would, so it is reported as unresolvable rather than guessed. */
export function resolveRoot(raw: string | null | undefined, home: string | null): ResolvedRoot {
	if (!raw || !raw.trim()) return { root: null, why: ROOT_UNKNOWN };
	const t = raw.trim();
	if (/[$%]/.test(t)) {
		return {
			root: null,
			why: `Root ${t} uses a variable, so it cannot be resolved here exactly as the backend would`,
		};
	}
	if (t === '~' || t.startsWith('~/') || t.startsWith('~\\')) {
		if (!home) return { root: null, why: 'Home directory unknown, so the target path cannot be checked' };
		return { root: home.replace(/[\\/]+$/, '') + t.slice(1) };
	}
	if (t.startsWith('~')) {
		return { root: null, why: `Root ${t} names another user's home, so it cannot be resolved here` };
	}
	if (!/^(\/|[A-Za-z]:[\\/]|\\\\)/.test(t)) {
		return { root: null, why: `Root ${t} is not an absolute path, so it cannot be resolved here` };
	}
	return { root: t };
}

export function samePath(a: string, b: string): boolean {
	return normPath(a) === normPath(b);
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

/** What removing this placement does, as DEC-30 requires. */
export function PlacementNature({ p, dir }: { p: NgwaPlacement; dir: boolean }) {
	if (p.mechanism === 'settings-key') {
		return (
			<p data-nature="settings">
				It is a settings entry inside <code>{p.path}</code>. The entry is removed from that file;
				the rest of the file is kept.
			</p>
		);
	}
	if (isLink(p)) {
		return (
			<p data-nature="link">
				It is a <b>symlink</b>
				{p.link_target ? (
					<>
						{' '}
						to <code>{p.link_target}</code>
					</>
				) : null}
				. Only the link is removed;{' '}
				{p.in_store ? 'the store copy survives.' : 'whatever it points at is left untouched.'}
			</p>
		);
	}
	return dir ? (
		<p data-nature="real-dir">
			It is a <b>real folder</b>, not a link. The folder <b>and everything in it</b> are{' '}
			<b>deleted permanently</b>.
		</p>
	) : (
		<p data-nature="real-file">
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
	/** Project root on disk; personal columns take the surface's `homeDir`. */
	root?: string | null;
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
		to: ClaudeStoreScope,
		/** `overwrite: true` only for DEC-31 "Update personal", after its confirm. */
		opts?: { overwrite?: boolean }
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
	/** The user's home directory — the personal scope root. `null` while it is
	 *  unresolved; every path-checked action is then disabled with a reason. */
	homeDir?: string | null;
	actions: NgwaScopeActions;
	kind?: string;
	onKindChange?: (kind: string) => void;
	search?: string;
	focusScope?: string;
	/** The snapshot is refetching: open menus would act on stale rows. */
	refreshing?: boolean;
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
	unknown: 'unknown — a source is unreadable',
};

/** Sources whose loss makes primitive cells unknowable (must-fix 2a). */
const PRIMITIVE_SOURCES = ['engine_config', 'oba'];

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

/** What is open. Items are rebuilt from the current rows on every render, so a
 *  refetch or a finished mutation can never leave a stale action clickable. */
interface OpenCell {
	id: string;
	rowKey: string;
	col: string;
	engine: boolean;
}

const BUSY_REASON = 'Waiting for the last change to finish and the matrix to refresh';

interface EnableTarget {
	label: string;
	go: () => Promise<unknown>;
}

export function NgwaScopesSurface({
	items,
	isLoading = false,
	error = null,
	unreadableSources = [],
	scopes,
	homeDir = null,
	actions,
	kind = '*',
	onKindChange,
	search,
	focusScope,
	refreshing = false,
}: NgwaScopesSurfaceProps) {
	const [openCell, setOpenCell] = useState<OpenCell | null>(null);
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
	const rootInfo = useCallback(
		(key: string): ResolvedRoot =>
			key === 'personal'
				? resolveRoot(homeDir, homeDir)
				: resolveRoot(scopes.find((s) => s.key === key)?.root ?? null, homeDir || null),
		[scopes, homeDir]
	);
	const rootOf = useCallback((key: string) => rootInfo(key).root, [rootInfo]);
	const rootWhy = useCallback((key: string) => rootInfo(key).why ?? ROOT_UNKNOWN, [rootInfo]);

	const primitivesDown = unreadableSources.filter((s) => PRIMITIVE_SOURCES.includes(s.source));
	const unknown = primitivesDown.length > 0;
	const unknownReason = unknown
		? `State unknown: ${primitivesDown.map((s) => s.source).join(', ')} unreadable`
		: undefined;

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

	/** Every scanned placement, for "is anything already at this path?". */
	const allPlacements = useMemo(() => items.flatMap((it) => it.placements), [items]);
	const atPath = useCallback(
		(path: string, sk: ClaudeStoreKind | null): NgwaPlacement | null =>
			allPlacements.find((p) => samePath(placementTarget(p, sk), path)) ?? null,
		[allPlacements]
	);

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

	const closePop = useCallback(() => setOpenCell(null), []);
	const busy = pending || refreshing;

	// ── Guards: every placing / deleting action checks the path it touches ──

	/** Why Enable here (claude, scope `key`) must not run, or undefined. */
	function enableBlock(row: MatrixRow, key: string): string | undefined {
		if (unknownReason) return unknownReason;
		const sk = row.storeKind;
		if (!sk) return 'This kind has no scope writer';
		const settings = sk === 'hook' || sk === 'mcp';
		const dest = settings ? null : expectedPath('claude', sk, row.name, rootOf(key));
		const there = dest ? atPath(dest, sk) : null;
		// place_primitive has no clobber guard: a real file at the target is the
		// first thing to rule out, and it is named, not folded into "enabled".
		if (dest && there && !isLink(there)) {
			return `A real ${isDirKind(sk) ? 'folder' : 'file'} is already at ${dest}; enabling would replace it`;
		}
		const mark = scopeMark(row, key, conflicts.get(row.key) ?? null);
		if (mark === 'on' || mark === 'link' || mark === 'conflict') return 'Already enabled here';
		if (!row.storeBacked) return 'Not in the Ọba store, so there is nothing to place; use Copy here';
		if (settings) return undefined;
		if (!dest) return rootWhy(key);
		if (there) return 'Already enabled here';
		return undefined;
	}

	/** The claude source a Move/Copy into `key` reads from, or a reason. */
	function moveSource(row: MatrixRow, key: string): { key: string; p: NgwaPlacement } | string {
		const sk = row.storeKind;
		const candidates = [...row.byScope.keys()].filter((k) => k !== key);
		candidates.sort((a, b) => (a === 'personal' ? -1 : b === 'personal' ? 1 : 0));
		for (const k of candidates) {
			const cp = claudePlacement(row.byScope.get(k), k);
			if (!cp) continue;
			if (sk === 'hook' || sk === 'mcp') return { key: k, p: cp };
			const exp = expectedPath('claude', sk, row.name, rootOf(k));
			if (exp && samePath(placementTarget(cp, sk), exp)) return { key: k, p: cp };
		}
		return 'Not placed for claude at a checkable path in any other scope';
	}

	/** Why Move/Copy into `key` must not run, or undefined. */
	function moveCopyBlock(row: MatrixRow, key: string): string | undefined {
		if (unknownReason) return unknownReason;
		const sk = row.storeKind;
		if (!sk) return 'This kind has no scope writer';
		const src = moveSource(row, key);
		if (typeof src === 'string') return src;
		if (sk === 'hook' || sk === 'mcp') {
			const here = row.byScope.get(key);
			return here && placementsIn(here, key).length > 0 ? 'Already present here' : undefined;
		}
		const dest = expectedPath('claude', sk, row.name, rootOf(key));
		if (!dest) return rootWhy(key);
		if (atPath(dest, sk)) return `Something is already at ${dest}`;
		return undefined;
	}

	/** The claude placement at the exact path Disable/Remove delete, or a reason. */
	function claudeTarget(row: MatrixRow, key: string): NgwaPlacement | string {
		if (unknownReason) return unknownReason;
		const sk = row.storeKind;
		const cp = claudePlacement(row.byScope.get(key), key);
		if (sk === 'hook' || sk === 'mcp') {
			return cp && cp.mechanism === 'settings-key' ? cp : 'Nothing placed for claude here';
		}
		const dest = expectedPath('claude', sk, row.name, rootOf(key));
		if (!dest) return rootWhy(key);
		const there = row.items
			.flatMap((it) => it.placements)
			.find((p) => p.engine === 'claude' && samePath(placementTarget(p, sk), dest));
		return there ?? `Nothing scanned at ${dest}`;
	}

	/** Engine Disable: the placement at the path `disable_for_core` deletes, and a symlink. */
	function engineDisableTarget(row: MatrixRow, engine: EngineId): { p: NgwaPlacement; key: string } | string {
		if (unknownReason) return unknownReason;
		const sk = row.storeKind;
		const mine = row.items.flatMap((it) => it.placements).filter((p) => p.engine === engine && p.present);
		if (mine.length === 0) return `Not placed for ${engine}`;
		if (sk === 'hook' || sk === 'mcp') {
			if (!row.storeBacked) return 'Not in the Ọba store; the entry cannot be matched to a store fragment';
			const p = mine.find((x) => x.mechanism === 'settings-key');
			return p ? { p, key: scopeKeyOf(p.scope) } : `Not placed for ${engine}`;
		}
		let why = `The ${engine} placement is not at the path the disable command deletes`;
		for (const p of mine) {
			const key = scopeKeyOf(p.scope);
			const dest = expectedPath(engine, sk, row.name, rootOf(key));
			if (!dest) {
				why = `The ${engine} target path for ${row.kind}s cannot be checked here`;
				continue;
			}
			if (!samePath(placementTarget(p, sk), dest)) continue;
			if (!isLink(p)) {
				why = `A real ${isDirKind(sk) ? 'folder' : 'file'}, not a store link; disabling would delete it`;
				continue;
			}
			return { p, key };
		}
		return why;
	}

	/** Engine Enable (personal): why it must not run, or undefined. */
	function engineEnableBlock(row: MatrixRow, engine: EngineId): string | undefined {
		if (unknownReason) return unknownReason;
		const sk = row.storeKind;
		if (row.pkg || !sk) return 'Not placed per engine';
		if (engineMark(row, engine) !== 'none') return `Already placed for ${engine}`;
		if (!row.storeBacked) return 'Not in the Ọba store, so there is nothing to place';
		if (sk === 'hook' || sk === 'mcp') return undefined;
		const dest = expectedPath(engine, sk, row.name, homeDir || null);
		if (!dest) {
			return homeDir
				? `The ${engine} target path for ${row.kind}s cannot be checked here`
				: 'Home directory unknown, so the target path cannot be checked';
		}
		const there = atPath(dest, sk);
		if (there && !isLink(there)) {
			return `A real ${isDirKind(sk) ? 'folder' : 'file'} is already at ${dest}; enabling would replace it`;
		}
		if (there) return `Already placed for ${engine}`;
		return undefined;
	}

	// ── Popover content ──
	function scopePopItems(row: MatrixRow, col: ScopeColumn): PopItem[] {
		const here = row.byScope.get(col.key);
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
					disabledReason: builtin
						? 'Shipped with the shell and cannot be uninstalled; disable it instead'
						: undefined,
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
		if (unknownReason) {
			return [
				{ label: 'Enable here', disabledReason: unknownReason },
				{ label: 'Move here', disabledReason: unknownReason },
				{ label: 'Copy here', disabledReason: unknownReason },
				{ sep: true, label: '' },
				{ label: 'Disable', disabledReason: unknownReason },
				{ label: `Remove from ${where}`, danger: true, disabledReason: unknownReason },
			];
		}
		const conflict = conflicts.get(row.key) ?? null;
		if (conflict && col.key === 'personal') return conflictPopItems(conflict);

		const src = moveSource(row, col.key);
		const mcBlock = moveCopyBlock(row, col.key);
		const target = claudeTarget(row, col.key);
		const settingsKind = sk === 'hook' || sk === 'mcp';
		const disableBlock =
			typeof target === 'string'
				? target
				: settingsKind && !row.storeBacked
					? `Not in the Ọba store: Disable would delete your own ${sk === 'mcp' ? 'MCP server' : 'hook'} entry from ${target.path}, including its settings. Use Remove from ${where} to delete it deliberately`
					: target.mechanism !== 'settings-key' && !isLink(target)
						? `A real ${isDirKind(sk) ? 'folder' : 'file'}, not a store link; disabling would delete it. Use Remove from ${where}`
						: undefined;
		return [
			{
				label: 'Enable here',
				disabledReason: enableBlock(row, col.key),
				onSelect: () =>
					void run(`Enabled ${row.label} in ${where}`, () => actions.enable(sk, row.name, scope)),
			},
			{
				label: 'Move here',
				sub: typeof src === 'string' ? undefined : `from ${scopeLabel(src.key)}`,
				disabledReason: mcBlock,
				onSelect: () => typeof src !== 'string' && setConfirm(moveRequest(row, sk, src, col.key)),
			},
			{
				label: 'Copy here',
				sub: typeof src === 'string' ? undefined : `from ${scopeLabel(src.key)}`,
				disabledReason: mcBlock,
				onSelect: () => typeof src !== 'string' && setConfirm(copyRequest(row, sk, src, col.key)),
			},
			{ sep: true, label: '' },
			{
				label: 'Disable',
				disabledReason: disableBlock,
				onSelect: () =>
					void run(`Disabled ${row.label} in ${where}`, () => actions.disable(sk, row.name, scope)),
			},
			{
				label: `Remove from ${where}`,
				danger: true,
				disabledReason: typeof target === 'string' ? target : undefined,
				onSelect: () =>
					typeof target !== 'string' && setConfirm(removeRequest(row, sk, col.key, target)),
			},
		];
	}

	function copyRequest(
		row: MatrixRow,
		sk: ClaudeStoreKind,
		src: { key: string; p: NgwaPlacement },
		toKey: string
	): ConfirmRequest {
		const settings = sk === 'hook' || sk === 'mcp';
		const from = settings ? src.p.path : placementTarget(src.p, sk);
		const to = settings ? null : expectedPath('claude', sk, row.name, rootOf(toKey));
		return {
			title: `Copy ${row.label} to ${scopeLabel(toKey)}`,
			confirmLabel: 'Copy',
			body: settings ? (
				<>
					<p>
						Writes the store copy of the entry into {scopeLabel(toKey)}. The source{' '}
						<code data-copy-source>{from}</code> is left as it is.
					</p>
				</>
			) : (
				<>
					<p>
						Copies <code data-copy-source>{from}</code> to <code data-copy-dest>{to}</code>. The source
						is left as it is.
					</p>
					<p>
						Nothing is overwritten: if anything already exists at the destination, including a
						folder this screen cannot see, the copy is refused.
					</p>
				</>
			),
			run: () => actions.copy(sk, row.name, wireOf(src.key), wireOf(toKey)),
		};
	}

	function moveRequest(
		row: MatrixRow,
		sk: ClaudeStoreKind,
		src: { key: string; p: NgwaPlacement },
		toKey: string
	): ConfirmRequest {
		const settings = sk === 'hook' || sk === 'mcp';
		const from = settings ? src.p.path : placementTarget(src.p, sk);
		const to = settings ? null : expectedPath('claude', sk, row.name, rootOf(toKey));
		return {
			title: `Move ${row.label} to ${scopeLabel(toKey)}`,
			confirmLabel: 'Move',
			body: settings ? (
				<>
					<p>
						Writes the store copy of the entry into {scopeLabel(toKey)}, then removes it from{' '}
						<code data-move-source>{from}</code>.
					</p>
					<p>Any local edits to the entry in that file are lost; the store fragment is what is written.</p>
				</>
			) : (
				<>
					<p>
						Copies <code data-move-source>{from}</code> to <code>{to}</code>, then <b>deletes the source</b>
						{isDirKind(sk) ? ' folder and everything in it' : ''}.
					</p>
					{isLink(src.p) ? (
						<p>
							The source is a store link, so the destination becomes a <b>standalone copy</b> that no
							longer receives store updates.
						</p>
					) : null}
					<p>
						Nothing is overwritten: if anything already exists at the destination, including a
						folder this screen cannot see, the move is refused and the source is kept.
					</p>
				</>
			),
			run: () => actions.move(sk, row.name, wireOf(src.key), wireOf(toKey)),
		};
	}

	function removeRequest(
		row: MatrixRow,
		sk: ClaudeStoreKind,
		scopeKey: string,
		p: NgwaPlacement
	): ConfirmRequest {
		const where = scopeLabel(scopeKey);
		const dir = isDirKind(sk) && p.mechanism !== 'settings-key';
		const target = p.mechanism === 'settings-key' ? p.path : placementTarget(p, sk);
		return {
			title: `Remove ${row.label} from ${where}`,
			confirmLabel: 'Remove',
			body: (
				<>
					<p>
						This removes {dir ? 'the folder ' : ''}
						<code data-remove-path>{target}</code>
						{p.mechanism === 'settings-key' ? ' (one entry)' : ''}.
					</p>
					<PlacementNature p={p} dir={dir} />
					<p>There is no undo.</p>
				</>
			),
			run: () => actions.remove(sk, row.name, wireOf(scopeKey)),
		};
	}

	function updatePersonalRequest(c: ScopeConflict): ConfirmRequest | null {
		const sk = c.row.storeKind;
		if (!sk || !c.project || unknownReason) return null;
		const personal = claudeTarget(c.row, 'personal');
		if (typeof personal === 'string') return null;
		const projKey = scopeKeyOf(c.project.scope);
		const dir = isDirKind(sk);
		// The path copy_core reads: resolve_scope_claude(project)/<kind>s/<name>.
		const from = expectedPath('claude', sk, c.row.name, rootOf(projKey));
		if (!from) return null;
		return {
			title: `Update personal ${c.row.label}`,
			confirmLabel: 'Overwrite personal',
			body: (
				<>
					<p>
						Copies the {scopeLabel(projKey)} version from <code data-update-source>{from}</code> over{' '}
						{dir ? 'the folder ' : ''}
						<code data-overwrite-path>{placementTarget(personal, sk)}</code>.
					</p>
					{isLink(personal) ? (
						<p data-nature="link">
							The personal copy is a <b>symlink</b>: the link is replaced by a real{' '}
							{dir ? 'folder' : 'file'} and{' '}
							{personal.in_store
								? 'the store copy survives.'
								: 'whatever it pointed at is left untouched.'}
						</p>
					) : dir ? (
						<p data-nature="real-dir">
							The personal copy is a <b>real folder</b>: the folder <b>and everything in it</b> are
							replaced.
						</p>
					) : (
						<p data-nature="real-file">
							The personal copy is a <b>real file</b>: its current contents are overwritten.
						</p>
					)}
					<p>There is no undo.</p>
				</>
			),
			// DEC-31: the one deliberate overwrite, behind this confirm.
			run: () => actions.copy(sk, c.row.name, wireOf(projKey), 'workspace', { overwrite: true }),
		};
	}

	function conflictPopItems(c: ScopeConflict): PopItem[] {
		const upd = updatePersonalRequest(c);
		const sk = c.row.storeKind;
		const target = claudeTarget(c.row, 'personal');
		return [
			{
				label: 'Update personal',
				disabledReason: upd
					? undefined
					: !c.project
						? `The shadowing copy (${c.shadowPath}) is not in a registered project`
						: typeof target === 'string'
							? target
							: rootWhy(scopeKeyOf(c.project.scope)),
				onSelect: () => upd && setConfirm(upd),
			},
			{
				label: 'Remove from Personal',
				danger: true,
				disabledReason: typeof target === 'string' ? target : undefined,
				onSelect: () =>
					typeof target !== 'string' &&
					sk &&
					setConfirm(removeRequest(c.row, sk, 'personal', target)),
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
		const cross = unknownReason ?? 'Cross-engine move and copy are not wired on this screen';
		const dis = engineDisableTarget(row, engine);
		return [
			{
				label: 'Enable here',
				sub: 'in Personal',
				disabledReason: engineEnableBlock(row, engine),
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
				sub: typeof dis === 'string' ? undefined : `in ${scopeLabel(dis.key)}`,
				disabledReason: typeof dis === 'string' ? dis : undefined,
				onSelect: () =>
					typeof dis !== 'string' &&
					void run(`Disabled ${row.label} for ${engine}`, () =>
						actions.disableFor(engine, sk, row.name, wireOf(dis.key))
					),
			},
		];
	}

	function popFor(title: string, list: PopItem[]): OpenPop {
		const items = busy
			? list.map((it) => (it.sep ? it : { ...it, disabledReason: BUSY_REASON }))
			: list;
		return { id: title, title, items };
	}

	// ── Enable all (D-02: confirm, then apply). Targets are exactly the rows
	// whose own cell action is allowed, so "untouched" is true by construction.
	function enableAllScope(col: ScopeColumn): EnableTarget[] {
		const scope = wireOf(col.key);
		const targets: EnableTarget[] = [];
		for (const row of rows) {
			const here = row.byScope.get(col.key);
			if (row.pkg) {
				if (here && here.state !== 'enabled') {
					targets.push({ label: row.label, go: () => actions.pkgSetEnabled(here.id, true) });
				}
			} else if (row.storeKind && enableBlock(row, col.key) === undefined) {
				const sk = row.storeKind;
				targets.push({ label: row.label, go: () => actions.enable(sk, row.name, scope) });
			}
		}
		return targets;
	}
	function enableAllEngine(engine: EngineId): EnableTarget[] {
		const targets: EnableTarget[] = [];
		for (const row of rows) {
			if (row.pkg || !row.storeKind || engineEnableBlock(row, engine) !== undefined) continue;
			const sk = row.storeKind;
			targets.push({
				label: row.label,
				go: () => actions.enableFor(engine, sk, row.name, 'workspace'),
			});
		}
		return targets;
	}
	function enableAllReason(where: string, targets: EnableTarget[]): string | undefined {
		if (unknownReason) return unknownReason;
		if (targets.length === 0) return `Every eligible row is already enabled in ${where}`;
		return undefined;
	}
	// The latest target builders, so a confirmed Enable all re-checks the
	// current rows instead of trusting the list captured when it opened.
	const latest = useRef({ scope: enableAllScope, engine: enableAllEngine });
	latest.current = { scope: enableAllScope, engine: enableAllEngine };

	function askEnableAll(where: string, targets: EnableTarget[], recompute: () => EnableTarget[]) {
		setConfirm({
			title: `Enable all in ${where}`,
			confirmLabel: `Enable ${targets.length}`,
			body: (
				<>
					<p>
						This enables <b>{targets.length}</b> item{targets.length === 1 ? '' : 's'} in <b>{where}</b>.
						Items already enabled there, and anything already on disk at a target path, are untouched.
					</p>
					<p>{targets.map((t) => t.label).join(', ')}</p>
				</>
			),
			run: async () => {
				const now = recompute();
				const was = targets.map((t) => t.label).join('\n');
				if (now.map((t) => t.label).join('\n') !== was) {
					throw new Error('The matrix changed since this was opened; nothing was enabled. Review it again');
				}
				const failed: string[] = [];
				for (const t of now) {
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
	const kindCounts = new Map<string, number>();
	for (const r of searchedRows) kindCounts.set(r.kind, (kindCounts.get(r.kind) ?? 0) + 1);

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			{unreadableSources.length > 0 && (
				<div className="source-banner" role="alert" data-unreadable>
					<AlertTriangle className="h-4 w-4" />
					<span>
						Unreadable:{' '}
						{unreadableSources.map((s) => `${s.source}${s.error ? ` (${s.error})` : ''}`).join('; ')}.
						Rows from those sources are missing, not absent
						{unknown ? ', and actions that place or delete files are disabled' : ''}.
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
										const why = enableAllReason(col.label, targets);
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
													disabled={busy || why !== undefined}
													title={busy ? BUSY_REASON : why}
													onClick={() => askEnableAll(col.label, targets, () => latest.current.scope(col))}
												>
													Enable all
												</button>
											</th>
										);
									})}
									{engineCols.map((eng) => {
										const targets = enableAllEngine(eng);
										const why = enableAllReason(eng, targets);
										return (
											<th key={eng} className="eng" data-col={eng}>
												<span className="colname">{eng}</span>
												<span className="colsub">{engines.get(eng)?.version ?? '—'}</span>
												<button
													type="button"
													className="chip enall"
													data-enall={eng}
													disabled={busy || why !== undefined}
													title={busy ? BUSY_REASON : why}
													onClick={() => askEnableAll(eng, targets, () => latest.current.engine(eng))}
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
												const mark = scopeMark(row, col.key, conflict, unknown);
												const v = row.byScope.get(col.key)?.version;
												return (
													<td key={col.key} className="cell">
														<MatrixCell
															id={id}
															mark={mark}
															version={mark === 'conflict' ? (v ?? '—') : undefined}
															label={`${row.label} in ${col.label}: ${MARK_TITLE[mark]}`}
															open={openCell?.id === id}
															onOpen={() =>
																setOpenCell(
																	openCell?.id === id
																		? null
																		: { id, rowKey: row.key, col: col.key, engine: false }
																)
															}
															pop={
																openCell?.id === id
																	? popFor(`${col.label} · ${row.label}`, scopePopItems(row, col))
																	: null
															}
															onClosePop={closePop}
														/>
													</td>
												);
											})}
											{engineCols.map((eng) => {
												const id = `${row.key}|${eng}`;
												const mark = engineMark(row, eng, unknown);
												return (
													<td key={eng} className="cell eng">
														<MatrixCell
															id={id}
															mark={mark}
															label={`${row.label} for ${eng}: ${MARK_TITLE[mark]}`}
															open={openCell?.id === id}
															onOpen={() =>
																setOpenCell(
																	openCell?.id === id
																		? null
																		: { id, rowKey: row.key, col: eng, engine: true }
																)
															}
															pop={
																openCell?.id === id
																	? popFor(`${eng} · ${row.label}`, enginePopItems(row, eng))
																	: null
															}
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
					{unknown ? (
						<div className="snote warn" data-conflicts-unknown>
							Conflicts unknown: {primitivesDown.map((s) => s.source).join(', ')} unreadable.
						</div>
					) : conflictList.length > 0 ? (
						conflictList.map((c) => {
							const projKey = c.project ? scopeKeyOf(c.project.scope) : null;
							const upd = updatePersonalRequest(c);
							const target = claudeTarget(c.row, 'personal');
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
												title={
													upd
														? undefined
														: typeof target === 'string'
															? target
															: 'The shadowing copy is not in a registered project'
												}
												onClick={() => upd && setConfirm(upd)}
											>
												Update personal
											</button>
											<button
												type="button"
												className="chip clear"
												data-act="remove-personal"
												disabled={typeof target === 'string' || !c.row.storeKind}
												title={typeof target === 'string' ? target : undefined}
												onClick={() =>
													typeof target !== 'string' &&
													c.row.storeKind &&
													setConfirm(removeRequest(c.row, c.row.storeKind, 'personal', target))
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
						<div>
							<HelpCircle className="h-3.5 w-3.5" />
							<span>unknown: its source is unreadable</span>
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
					setOpenCell(null);
					if (result?.ok) setStatus({ tone: 'ok', text: `${title}: done` });
					else if (result && !result.ok)
						setStatus({ tone: 'err', text: `${title} failed: ${result.error}` });
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
		case 'unknown':
			return <HelpCircle className="h-3.5 w-3.5" />;
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
