// Ngwa scope operations (WP-16a guards, shared since the Installed tab / item
// detail wiring).
//
// Every check the Scopes matrix runs before it places, moves, copies,
// disables or removes something, plus the DEC-30 confirm requests those
// actions sit behind. Lifted out of `NgwaScopesSurface` unchanged so the
// Installed tab and the item detail act through the exact same guards and
// the same confirm copy instead of a second, drifting copy of them.
//
// `createScopeOps` is pure over its input; `useScopeOps` memoises it.

import { useEffect, useMemo, useRef, type CSSProperties, type RefObject } from 'react';
import type { NgwaItem, NgwaPlacement } from '@ikenga/contract';
import type { ClaudeStoreKind, EngineId } from '@/lib/tauri-cmd';
import { fullyDown, notScannedReason, rootNotScanned } from '@/lib/ngwa/scan-coverage';
import {
	PlacementNature,
	ROOT_UNKNOWN,
	buildRows,
	claudePlacement,
	conflictOf,
	engineMark,
	expectedPath,
	isDirKind,
	isLink,
	placementTarget,
	placementsIn,
	resolveRoot,
	samePath,
	scopeKeyOf,
	scopeMark,
	wireOf,
	type ConfirmRequest,
	type MatrixRow,
	type NgwaScopeActions,
	type ResolvedRoot,
	type ScopeColumn,
	type ScopeConflict,
} from './ngwa-scope-model';

/** One row of a Scopes cell popover / Installed context menu / scope picker. */
export interface PopItem {
	label: string;
	sub?: string;
	/** Right-aligned key hint (D-02 row menu: `Space`, `↵`). */
	k?: string;
	danger?: boolean;
	disabledReason?: string;
	onSelect?: () => void;
	sep?: boolean;
	/** A non-interactive group header (D-02 row menu: the pkg name). */
	group?: boolean;
	/** Selecting it swaps the menu's content instead of closing it. */
	keepOpen?: boolean;
}

export const BUSY_REASON = 'Waiting for the last change to finish and the matrix to refresh';

/** Sources whose loss makes primitive cells unknowable (must-fix 2a). */
export const PRIMITIVE_SOURCES = ['engine_config', 'oba'];

/** Scopes wording for a pkg: it is never copied or moved between scopes. */
export const PKG_ONE_SCOPE = 'A pkg lives in exactly one scope';
/** Scopes wording for Remove on a shell-bundled pkg. */
export const BUILTIN_REMOVE_REASON =
	'Shipped with the shell and cannot be uninstalled; disable it instead';

export interface ScopeOpsInput {
	items: readonly NgwaItem[];
	scopes: readonly ScopeColumn[];
	/** The personal scope root; `null` while unresolved. */
	homeDir: string | null;
	unreadableSources: ReadonlyArray<{ source: string; error: string | null }>;
	actions: NgwaScopeActions;
}

export type ScopeOps = ReturnType<typeof createScopeOps>;

export function createScopeOps({
	items,
	scopes,
	homeDir,
	unreadableSources,
	actions,
}: ScopeOpsInput) {
	const scopeLabel = (key: string) => scopes.find((s) => s.key === key)?.label ?? key;
	const rootInfo = (key: string): ResolvedRoot =>
		key === 'personal'
			? resolveRoot(homeDir, homeDir)
			: resolveRoot(scopes.find((s) => s.key === key)?.root ?? null, homeDir || null);
	const rootOf = (key: string) => rootInfo(key).root;
	const rootWhy = (key: string) => rootInfo(key).why ?? ROOT_UNKNOWN;

	// A partially unreadable scan (it skipped a root or a file) leaves the
	// rows it read knowable; only a source that failed outright makes every
	// primitive cell unknown.
	const primitivesDown = fullyDown(
		unreadableSources.filter((s) => PRIMITIVE_SOURCES.includes(s.source))
	);
	const unknownReason =
		primitivesDown.length > 0
			? `State unknown: ${primitivesDown.map((s) => s.source).join(', ')} unreadable`
			: undefined;
	// A project column whose root the config scan did not read (on the
	// daemon: outside the fs allowlist): its state is unknown, not empty.
	const configError = unreadableSources.find((s) => s.source === 'engine_config')?.error ?? null;
	function notScanned(key: string): string | undefined {
		if (key === 'personal') return undefined;
		const col = scopes.find((s) => s.key === key);
		const why = rootNotScanned(configError, col?.root);
		return why === null ? undefined : notScannedReason(col?.label ?? key, why);
	}

	const allRows = buildRows(items);
	const conflicts = new Map<string, ScopeConflict>();
	for (const r of allRows) {
		const c = conflictOf(r);
		if (c) conflicts.set(r.key, c);
	}

	/** Every scanned placement, for "is anything already at this path?". */
	const allPlacements = items.flatMap((it) => it.placements);
	const atPath = (path: string, sk: ClaudeStoreKind | null): NgwaPlacement | null =>
		allPlacements.find((p) => samePath(placementTarget(p, sk), path)) ?? null;

	/** The matrix row an item belongs to. */
	function rowOf(it: NgwaItem): MatrixRow | null {
		return allRows.find((r) => r.items.some((x) => x.id === it.id)) ?? null;
	}

	// ── Guards: every placing / deleting action checks the path it touches ──

	/** Why Enable here (claude, scope `key`) must not run, or undefined. */
	function enableBlock(row: MatrixRow, key: string): string | undefined {
		if (unknownReason) return unknownReason;
		const unscanned = notScanned(key);
		if (unscanned) return unscanned;
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
		if (!row.storeBacked)
			return 'Not in the Ọba store, so there is nothing to place; use Copy here';
		if (settings) return undefined;
		if (!dest) return rootWhy(key);
		if (there) return 'Already enabled here';
		return undefined;
	}

	/** The claude source a Move/Copy into `key` reads from, or a reason. */
	/** `from` pins the source scope (the Installed tab moves the selected item
	 *  out of its own scope); without it the matrix picks personal first. */
	function moveSource(
		row: MatrixRow,
		key: string,
		from?: string
	): { key: string; p: NgwaPlacement } | string {
		const sk = row.storeKind;
		const candidates = [...row.byScope.keys()].filter(
			(k) => k !== key && (from === undefined || k === from)
		);
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
	function moveCopyBlock(row: MatrixRow, key: string, from?: string): string | undefined {
		if (unknownReason) return unknownReason;
		const unscanned = notScanned(key);
		if (unscanned) return unscanned;
		const sk = row.storeKind;
		if (!sk) return 'This kind has no scope writer';
		const src = moveSource(row, key, from);
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
		const unscanned = notScanned(key);
		if (unscanned) return unscanned;
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
	function engineDisableTarget(
		row: MatrixRow,
		engine: EngineId
	): { p: NgwaPlacement; key: string } | string {
		if (unknownReason) return unknownReason;
		const sk = row.storeKind;
		const mine = row.items
			.flatMap((it) => it.placements)
			.filter((p) => p.engine === engine && p.present);
		if (mine.length === 0) return `Not placed for ${engine}`;
		if (sk === 'hook' || sk === 'mcp') {
			if (!row.storeBacked)
				return 'Not in the Ọba store; the entry cannot be matched to a store fragment';
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

	/** Why Disable (claude, scope `key`) must not run, or undefined. The same
	 *  checks the Scopes cell popover's Disable runs. */
	function disableBlock(row: MatrixRow, key: string): string | undefined {
		const sk = row.storeKind;
		if (!sk) return 'This kind has no scope writer';
		const where = scopeLabel(key);
		const target = claudeTarget(row, key);
		const settingsKind = sk === 'hook' || sk === 'mcp';
		return typeof target === 'string'
			? target
			: settingsKind && !row.storeBacked
				? `Not in the Ọba store: Disable would delete your own ${sk === 'mcp' ? 'MCP server' : 'hook'} entry from ${target.path}, including its settings. Use Remove from ${where} to delete it deliberately`
				: target.mechanism !== 'settings-key' && !isLink(target)
					? `A real ${isDirKind(sk) ? 'folder' : 'file'}, not a store link; disabling would delete it. Use Remove from ${where}`
					: undefined;
	}

	/** DEC-30 confirm for uninstalling a kernel pkg from `where`. */
	function pkgUninstallRequest(label: string, here: NgwaItem, where: string): ConfirmRequest {
		return {
			title: `Uninstall ${label}`,
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
		};
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
						Copies <code data-copy-source>{from}</code> to <code data-copy-dest>{to}</code>. The
						source is left as it is.
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
					<p>
						Any local edits to the entry in that file are lost; the store fragment is what is
						written.
					</p>
				</>
			) : (
				<>
					<p>
						Copies <code data-move-source>{from}</code> to <code>{to}</code>, then{' '}
						<b>deletes the source</b>
						{isDirKind(sk) ? ' folder and everything in it' : ''}.
					</p>
					{isLink(src.p) ? (
						<p>
							The source is a store link, so the destination becomes a <b>standalone copy</b> that
							no longer receives store updates.
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
						Copies the {scopeLabel(projKey)} version from <code data-update-source>{from}</code>{' '}
						over {dir ? 'the folder ' : ''}
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

	return {
		allRows,
		conflicts,
		unknownReason,
		notScanned,
		scopeLabel,
		rootOf,
		rootWhy,
		atPath,
		rowOf,
		enableBlock,
		moveSource,
		moveCopyBlock,
		claudeTarget,
		disableBlock,
		engineDisableTarget,
		engineEnableBlock,
		copyRequest,
		moveRequest,
		removeRequest,
		updatePersonalRequest,
		pkgUninstallRequest,
	};
}

export function useScopeOps(input: ScopeOpsInput): ScopeOps {
	const { items, scopes, homeDir, unreadableSources, actions } = input;
	return useMemo(
		() => createScopeOps({ items, scopes, homeDir, unreadableSources, actions }),
		[items, scopes, homeDir, unreadableSources, actions]
	);
}

// ─── Menu popover (Scopes cells, Installed pickers + row context menu) ───────

export interface OpenPop {
	id: string;
	title: string;
	items: PopItem[];
}

/** The `.cellpop` menu. Items are rebuilt by the caller on every render, so a
 *  refetch or a finished mutation never leaves a stale action clickable. A
 *  disabled item carries its reason as its title (interaction spec §1.2). */
export function NgwaPopMenu({
	pop,
	anchor,
	onClose,
	className,
	style,
	autoFocus = true,
}: {
	pop: OpenPop;
	anchor?: RefObject<HTMLElement | null>;
	onClose: () => void;
	className?: string;
	style?: CSSProperties;
	autoFocus?: boolean;
}) {
	const ref = useRef<HTMLDivElement>(null);
	useEffect(() => {
		if (autoFocus) {
			const first = ref.current?.querySelector<HTMLButtonElement>('button:not([disabled])');
			first?.focus();
		}
		function onKey(e: KeyboardEvent) {
			if (e.key === 'Escape') {
				e.preventDefault();
				onClose();
				anchor?.current?.focus();
			}
		}
		function onDown(e: MouseEvent) {
			const t = e.target as Node;
			if (ref.current?.contains(t) || anchor?.current?.contains(t)) return;
			onClose();
		}
		document.addEventListener('keydown', onKey);
		document.addEventListener('mousedown', onDown);
		return () => {
			document.removeEventListener('keydown', onKey);
			document.removeEventListener('mousedown', onDown);
		};
	}, [onClose, anchor, autoFocus]);
	const hasGroup = pop.items.some((it) => it.group);
	return (
		<div
			ref={ref}
			className={`cellpop${className ? ` ${className}` : ''}`}
			style={style}
			role="menu"
			aria-label={pop.title}
			data-cellpop
		>
			{!hasGroup && <div className="mgroup">{pop.title}</div>}
			{pop.items.map((it, i) =>
				it.group ? (
					<div key={`grp-${i}`} className="mgroup">
						{it.label}
					</div>
				) : it.sep ? (
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
							if (!it.keepOpen) onClose();
						}}
					>
						<span>{it.label}</span>
						{it.sub && <span className="msub"> {it.sub}</span>}
						{it.k && <kbd className="mk">{it.k}</kbd>}
					</button>
				)
			)}
		</div>
	);
}
