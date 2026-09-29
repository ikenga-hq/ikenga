// Ngwa scope model (WP-16 / WP-16a / locked D-02), shared by the Scopes
// matrix, the Health surface, the Installed tab and the item detail.
//
// Pure helpers over the snapshot (rows, marks, target paths, root
// resolution), the DEC-30 confirm dialog, and the action contracts. Moved out
// of `ngwa-scopes-surface.tsx` unchanged so `ngwa-scope-ops.tsx` can build on
// it without an import cycle; the surface re-exports everything here.

import { useState, type ReactNode } from 'react';
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

export function placementsIn(it: NgwaItem, scopeKey: string): NgwaPlacement[] {
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

export const ROOT_UNKNOWN = 'Scope root unknown, so the target path cannot be checked';

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
		if (!home)
			return { root: null, why: 'Home directory unknown, so the target path cannot be checked' };
		return { root: home.replace(/[\\/]+$/, '') + t.slice(1) };
	}
	if (t.startsWith('~')) {
		return {
			root: null,
			why: `Root ${t} names another user's home, so it cannot be resolved here`,
		};
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
	/** Defaults to "Cancel" (D-02's Remove confirm says "Keep it"). */
	cancelLabel?: string;
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
							{request.cancelLabel ?? 'Cancel'}
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

// ─── Action contracts ────────────────────────────────────────────────────────

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
