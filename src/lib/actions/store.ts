// ═══════════════════════════════════════════════════════════════════════════
// G-ACTIONS-API — the effective model of actions, keys and menus (WP-52)
// ═══════════════════════════════════════════════════════════════════════════
//
// The one TypeScript API every Phase 6 UI binds to: menu renderers (WP-55),
// the dispatcher (WP-54), the D-06 tabs (WP-57..61) and the iyke surface
// (WP-62). Contract: `plans/shell-ux-rearchitecture/drafts/actions-schema.md`
// (G-ACTIONS, frozen Round 39). Pure merge: `merge.ts` / `menus.ts` /
// `registry.ts`; file I/O: WP-50's `client.ts`; runs: WP-53's runner.
//
// ── Sources and layers (G-ACTIONS §2.1) ─────────────────────────────────────
//   default   built-ins (`registry.ts` catalog) + `DEFAULT_KEYMAP` + §1.3 menus
//   package   `ui.context_actions[]` + `ui.command_palette[]` (kernel snapshot
//             `registries.context_actions` incl. its `command_palette` list),
//             ids `${pkg_id}:${id}`, grant order = installed_at, pkg id,
//             declaration (context actions before palette entries)
//   personal  ~/.ikenga/{actions,keybindings}.json
//   project   <root>/.ikenga/{actions,keybindings}.json — keybindings HELD
//             until trusted (DEC-65); menus apply before trust
//
// Keymap source naming: `KeymapEntry.source` is
// `'default' | 'package' | 'personal' | 'project'` — the pre-12c `'user'`
// member was renamed `'personal'` (Round 41 hand-off 2).
//
// ── Reading ─────────────────────────────────────────────────────────────────
//   type EffectiveModel = {
//     actions: EffectiveAction[]            // builtin, package, personal, project
//     actionById: ReadonlyMap<string, EffectiveAction>
//     shadowedActions: EffectiveAction[]    // personal ids the project redefines
//     keymap: EffectiveKeymap
//     menus: EffectiveMenus                 // { ids: string[]; get(menuId) }
//     issues: ModelIssue[]                  // merge-level §1.6 issues
//     files: ActionsFilesResult | null      // WP-50 read (per-file validation)
//     projectId: string | null; projectRoot: string | null
//   }
//   type EffectiveAction = {
//     id; name; icon?; description
//     source: 'builtin' | 'package' | 'personal' | 'project'
//     run: { kind: 'builtin' } | PackageRun (dispatch | view) | ActionRun (§8.1)
//     placements: { at: MenuId; when?; condition? }[]
//     locked; danger; hosted; osOnly; editable
//     pkgId?; packageOrigin?: 'context_action' | 'command_palette'
//     selector?; keyRequest?; userAction?; fileIndex?; overriddenBy?: 'project'
//   }
//   type EffectiveKeymap = {
//     entries: KeymapEntry[]      // merge order; what getKeymap() returns;
//                                 // platformOnly narrowed per platform
//     held: HeldKeybinding[]      // DEC-65 {index, rule, trust}
//     projectHeld: boolean
//     packageRequests: PackageKeyRequest[]
//                                 // {actionId, pkgId, key, when, origin,
//                                 //  byPlatform: {mac, other}: granted |
//                                 //  held {heldBy: KeyHolder} | invalid}
//     negatives: NegativeRuleResult[]   // {scope, index, rule, removed}
//     conflicts: { mac: KeymapConflicts; other: KeymapConflicts }
//                                 // KeymapConflicts = {clashes, precedence}
//                                 // (an object, not an array — Round 41 h/o 4)
//   }
//   type EffectiveMenu = {
//     id; items: (EffectiveMenuActionItem | {kind:'separator'})[]
//     hidden: string[]; overrides: {personal?, project?}
//   }
//   type EffectiveMenuActionItem = {
//     kind: 'action'; id; action: EffectiveAction
//     layer: 'default' | 'package' | 'personal' | 'project'
//     when?  (placement when — evaluate with buildMenuContext, §1.3)
//     condition?: MenuItemCondition (§1.3 annotation; false = skip)
//     display?: 'checkbox' | 'radio' | 'submenu'; group?; locked
//   }
//
//   getEffectiveModel(): EffectiveModel
//   getEffectiveActions(): EffectiveAction[]
//   getEffectiveAction(id: string): EffectiveAction | undefined
//   getEffectiveKeymap(): EffectiveKeymap
//   getEffectiveMenu(menuId: string): EffectiveMenu | null
//   useEffectiveModel(): EffectiveModel                  (React; starts the store)
//   useEffectiveActions(): EffectiveAction[]
//   useEffectiveKeymap(): EffectiveKeymap
//   useEffectiveMenu(menuId: string): EffectiveMenu | null
//   useActionsStore  — the zustand store: { status, model, error, version }
//   `getKeymap()` (`@/lib/keymap/registry`) returns `model.keymap.entries`
//   once published; `subscribeKeymap()` fires on every publish.
//   resolveKeypressWinner(candidates, keymap) — §2.3 (from `merge.ts`)
//
// ── Lifecycle and change subscription ───────────────────────────────────────
//   startActionsStore(): Promise<EffectiveModel>   idempotent; first read +
//       subscriptions: `actions://changed` (watcher + trust grants/revokes),
//       `pkg-installed` / `pkg-uninstalled` / `pkg-reloaded`, and the active
//       project. Every one re-merges without a restart. The hooks call it.
//   stopActionsStore(): void                       tears down; getKeymap()
//       reverts to the defaults
//   refreshActionsModel(): Promise<EffectiveModel> re-read files + packages
//   subscribeEffectiveModel(listener: (model) => void): () => void
//
// ── Writes (every write goes through the WP-50 validator; a refused write
//    throws `ActionsValidationError` and leaves the file untouched; a file
//    that is malformed on disk is never overwritten from its stale in-force
//    copy — `ActionsFileNotWritableError`; each write re-merges) ─────────────
//   saveUserAction(scope, action: UserAction): Promise<void>        upsert by id
//   deleteUserAction(scope, id): Promise<void>
//   resetUserActions(scope): Promise<void>          deletes `actions`
//   setMenuOverride(scope, menuId, override: MenuOverride | null)   null = reset
//   resetMenuOverride(scope, menuId): Promise<void> deletes that menu's key
//   resetMenuOverrides(scope): Promise<void>        deletes `menus`
//   hideAction(scope, id, menuIds?): Promise<void>  adds to `hidden` of every
//       menu containing it (or menuIds); keys untouched (DEC-58); a locked
//       id throws `LockedActionError`
//   unhideAction(scope, id, menuIds?): Promise<void>
//   addKeybinding(scope, rule: KeybindingRule): Promise<void>
//   rebindKey(scope, entry: KeymapEntry, newKey, opts?: {when?}): Promise<void>
//       entry's own rule at `scope` → edited in place; otherwise one negative
//       rule for the old key + one positive rule for the new key (§1.5)
//   unbindKey(scope, entry: KeymapEntry): Promise<void>
//       own rule at `scope` → removed; otherwise one negative rule
//   resetKeyOverride(scope, command): Promise<void> removes every rule for
//       `command` at `scope` (never writes the default back)
//   removeKeybinding(scope, index): Promise<void>
//   resetKeybindings(scope): Promise<void>          deletes `bindings`
//   `scope` is `'personal' | 'project'`. A project keybindings write changes
//   the file's hash, so its rules are held again until re-trusted (DEC-65).
// ═══════════════════════════════════════════════════════════════════════════

import { useEffect } from 'react';
import { create } from 'zustand';
import type { KeymapEntry } from '@/lib/keymap/defaults';
import { setEffectiveKeymap } from '@/lib/keymap/registry';
import { normalizeWhen } from '@/lib/keymap/when';
import { useShellStore } from '@/lib/shell/shell-store';
import { listen, pkgKernelStatus, type UnlistenFn } from '@/lib/tauri-cmd';
import {
	ACTIONS_SCHEMA,
	type ActionsDocument,
	type ActionsFilesResult,
	type ActionsFileState,
	type ActionsScope,
	type ActionsScopeFiles,
	type KeybindingRule,
	type KeybindingsDocument,
	KEYBINDINGS_SCHEMA,
	type MenuOverride,
	readActionsFiles,
	type UserAction,
	watchActionsFiles,
	writeActionsFile,
	writeKeybindingsFile,
} from './client';
import { buildEffectiveModel, type EffectiveKeymap, type EffectiveModel } from './merge';
import type { EffectiveMenu } from './menus';
import { type EffectiveAction, isLockedAction, type PackageActionSource, readPackageActions } from './registry';

export type {
	EffectiveKeymap,
	EffectiveMenus,
	EffectiveModel,
	HeldKeybinding,
	KeyHolder,
	KeyRequestPlatformStatus,
	ModelIssue,
	NegativeRuleResult,
	PackageKeyRequest,
} from './merge';
export { LAYER_RANK, resolveKeypressWinner } from './merge';
export type {
	EffectiveMenu,
	EffectiveMenuActionItem,
	EffectiveMenuItem,
	EffectiveMenuSeparator,
	MenuLayer,
} from './menus';
export { collapseSeparators, isKnownMenuId, SEPARATOR } from './menus';
export type {
	ActionSource,
	BuiltinRun,
	ContextSelector,
	EffectiveAction,
	EffectiveRun,
	MenuItemCondition,
	PackageRun,
	ResolvedPlacement,
} from './registry';
export { isLockedAction, LOCKED_ACTION_IDS } from './registry';

// ─── Errors ──────────────────────────────────────────────────────────────────

/** Hiding a never-hide item (§9.2). Nothing was written. */
export class LockedActionError extends Error {
	readonly actionId: string;
	constructor(actionId: string) {
		super(`\`${actionId}\` is locked: it can be reordered, never hidden`);
		this.name = 'LockedActionError';
		this.actionId = actionId;
	}
}

/** The target file can't be written from the model: the scope has no file
 *  (a path-less project), or the file on disk is malformed / unreadable and
 *  the model holds only its last valid copy (§1.1 — fix the file first). */
export class ActionsFileNotWritableError extends Error {
	readonly scope: ActionsScope;
	constructor(scope: ActionsScope, message: string) {
		super(message);
		this.name = 'ActionsFileNotWritableError';
		this.scope = scope;
	}
}

// ─── Store ───────────────────────────────────────────────────────────────────

export type ActionsStoreStatus = 'idle' | 'loading' | 'ready' | 'error';

export interface ActionsStoreState {
	status: ActionsStoreStatus;
	/** Always present: before the first read it is the defaults-only model. */
	model: EffectiveModel;
	/** The last read failure (the previous model stays in force). */
	error: string | null;
	/** Bumped on every re-merge. */
	version: number;
}

const EMPTY_MODEL = buildEffectiveModel({ files: null, packages: [] });

export const useActionsStore = create<ActionsStoreState>(() => ({
	status: 'idle',
	model: EMPTY_MODEL,
	error: null,
	version: 0,
}));

interface Inputs {
	files: ActionsFilesResult | null;
	packages: PackageActionSource[];
}

let inputs: Inputs = { files: null, packages: [] };
let started: Promise<EffectiveModel> | null = null;
let teardown: Array<() => void> = [];
let filesSeq = 0;
let packagesSeq = 0;

function publish(): EffectiveModel {
	const model = buildEffectiveModel(inputs);
	setEffectiveKeymap(model.keymap.entries);
	useActionsStore.setState((s) => ({ model, status: 'ready', error: null, version: s.version + 1 }));
	return model;
}

function fail(err: unknown): void {
	const message = err instanceof Error ? err.message : String(err);
	console.warn('[actions] effective model read failed:', message);
	useActionsStore.setState({ status: 'error', error: message });
}

async function loadFiles(): Promise<boolean> {
	const seq = ++filesSeq;
	const files = await readActionsFiles(null);
	if (seq !== filesSeq) return false;
	inputs = { ...inputs, files };
	return true;
}

async function loadPackages(): Promise<boolean> {
	const seq = ++packagesSeq;
	let packages: PackageActionSource[];
	try {
		packages = readPackageActions(await pkgKernelStatus());
	} catch (err) {
		// No kernel (vite-only dev) or a failed snapshot: no package layer.
		console.warn('[actions] pkg_kernel_status failed; package layer empty:', err);
		packages = [];
	}
	if (seq !== packagesSeq) return false;
	inputs = { ...inputs, packages };
	return true;
}

/** Re-reads the files and the package snapshot, then re-merges. */
export async function refreshActionsModel(): Promise<EffectiveModel> {
	try {
		await Promise.all([loadFiles(), loadPackages()]);
		return publish();
	} catch (err) {
		fail(err);
		return useActionsStore.getState().model;
	}
}

async function refreshFiles(): Promise<void> {
	try {
		if (await loadFiles()) publish();
	} catch (err) {
		fail(err);
	}
}

async function refreshPackages(): Promise<void> {
	if (await loadPackages()) publish();
}

/**
 * Starts the store: first read and every re-merge subscription. Idempotent —
 * concurrent and repeated calls share one start. Resolves with the first
 * model (the defaults-only model if the first read failed).
 */
export function startActionsStore(): Promise<EffectiveModel> {
	if (started) return started;
	useActionsStore.setState({ status: 'loading' });
	const subscriptions: Array<Promise<UnlistenFn>> = [
		// On-disk edits (250 ms debounced in Rust) and trust grants/revokes
		// (`reason: 'trust'`, DEC-65) both change what is in force.
		watchActionsFiles(() => refreshFiles()),
		listen('pkg-installed', () => void refreshPackages()),
		listen('pkg-uninstalled', () => void refreshPackages()),
		listen('pkg-reloaded', () => void refreshPackages()),
	];
	const unsubscribeProject = useShellStore.subscribe((state, prev) => {
		const a = state.activeProject;
		const b = prev.activeProject;
		if (a?.id !== b?.id || a?.root_path !== b?.root_path) void refreshFiles();
	});
	teardown = [
		unsubscribeProject,
		() => {
			for (const sub of subscriptions) {
				void sub.then((stop) => stop()).catch(() => {});
			}
		},
	];
	for (const sub of subscriptions) {
		sub.catch((err) => console.warn('[actions] subscription failed:', err));
	}
	started = refreshActionsModel();
	return started;
}

/** Stops every subscription and reverts `getKeymap()` to the defaults. */
export function stopActionsStore(): void {
	for (const stop of teardown) stop();
	teardown = [];
	started = null;
	filesSeq++;
	packagesSeq++;
	inputs = { files: null, packages: [] };
	setEffectiveKeymap(null);
	useActionsStore.setState({ status: 'idle', model: EMPTY_MODEL, error: null, version: 0 });
}

/** Called after every re-merge with the new model. */
export function subscribeEffectiveModel(listener: (model: EffectiveModel) => void): () => void {
	return useActionsStore.subscribe((state, prev) => {
		if (state.model !== prev.model) listener(state.model);
	});
}

// ─── Reads ───────────────────────────────────────────────────────────────────

export function getEffectiveModel(): EffectiveModel {
	return useActionsStore.getState().model;
}

export function getEffectiveActions(): EffectiveAction[] {
	return getEffectiveModel().actions;
}

export function getEffectiveAction(id: string): EffectiveAction | undefined {
	return getEffectiveModel().actionById.get(id);
}

export function getEffectiveKeymap(): EffectiveKeymap {
	return getEffectiveModel().keymap;
}

export function getEffectiveMenu(menuId: string): EffectiveMenu | null {
	return getEffectiveModel().menus.get(menuId);
}

function useStarted(): void {
	useEffect(() => {
		void startActionsStore();
	}, []);
}

export function useEffectiveModel(): EffectiveModel {
	useStarted();
	return useActionsStore((s) => s.model);
}

export function useEffectiveActions(): EffectiveAction[] {
	useStarted();
	return useActionsStore((s) => s.model.actions);
}

export function useEffectiveKeymap(): EffectiveKeymap {
	useStarted();
	return useActionsStore((s) => s.model.keymap);
}

export function useEffectiveMenu(menuId: string): EffectiveMenu | null {
	useStarted();
	// `menus.get` memoizes per model, so the selector result is stable.
	return useActionsStore((s) => s.model.menus.get(menuId));
}

// ─── Writes ──────────────────────────────────────────────────────────────────

function clone<T>(value: T): T {
	return JSON.parse(JSON.stringify(value)) as T;
}

function writableState<D extends ActionsDocument | KeybindingsDocument>(
	scope: ActionsScope,
	pick: (files: ActionsScopeFiles) => ActionsFileState<D>
): ActionsFileState<D> {
	const files = getEffectiveModel().files;
	if (!files) throw new ActionsFileNotWritableError(scope, 'the actions model has not loaded yet');
	const scoped = scope === 'personal' ? files.personal : files.project;
	if (!scoped) throw new ActionsFileNotWritableError(scope, 'the active project has no filesystem root');
	const state = pick(scoped);
	if (state.stale || state.error) {
		throw new ActionsFileNotWritableError(
			scope,
			`${state.path} is not in force as written (${state.error ?? 'invalid on disk'}); fix or reset the file first`
		);
	}
	return state;
}

async function editActions(scope: ActionsScope, edit: (doc: ActionsDocument) => void): Promise<void> {
	const state = writableState(scope, (f) => f.actions);
	const doc: ActionsDocument = state.document
		? clone(state.document)
		: { $schema: ACTIONS_SCHEMA, version: 1 };
	edit(doc);
	await writeActionsFile(scope, doc, getEffectiveModel().projectId);
	await refreshFiles();
}

async function editKeybindings(scope: ActionsScope, edit: (doc: KeybindingsDocument) => void): Promise<void> {
	const state = writableState(scope, (f) => f.keybindings);
	const doc: KeybindingsDocument = state.document
		? clone(state.document)
		: { $schema: KEYBINDINGS_SCHEMA, version: 1 };
	edit(doc);
	await writeKeybindingsFile(scope, doc, getEffectiveModel().projectId);
	await refreshFiles();
}

/** Upserts a user action by id (its `scope` is set to the file's, §1.2). */
export function saveUserAction(scope: ActionsScope, action: UserAction): Promise<void> {
	return editActions(scope, (doc) => {
		const next: UserAction = { ...clone(action), scope };
		const list = doc.actions ?? [];
		const at = list.findIndex((a) => a.id === next.id);
		if (at >= 0) list[at] = next;
		else list.push(next);
		doc.actions = list;
	});
}

export function deleteUserAction(scope: ActionsScope, id: string): Promise<void> {
	return editActions(scope, (doc) => {
		if (doc.actions) doc.actions = doc.actions.filter((a) => a.id !== id);
	});
}

/** Reset this tab (Actions): deletes `actions` — never writes defaults. */
export function resetUserActions(scope: ActionsScope): Promise<void> {
	return editActions(scope, (doc) => {
		delete doc.actions;
	});
}

/** Sets (or, with `null`, deletes = "Reset this menu") one menu override.
 *  A locked id in `hidden` throws before anything is written. */
export function setMenuOverride(scope: ActionsScope, menuId: string, override: MenuOverride | null): Promise<void> {
	const locked = override?.hidden?.find((id) => isLockedAction(id));
	if (locked) return Promise.reject(new LockedActionError(locked));
	return editActions(scope, (doc) => {
		const menus = { ...(doc.menus ?? {}) };
		if (override) menus[menuId] = clone(override);
		else delete menus[menuId];
		if (Object.keys(menus).length > 0) doc.menus = menus;
		else delete doc.menus;
	});
}

export function resetMenuOverride(scope: ActionsScope, menuId: string): Promise<void> {
	return setMenuOverride(scope, menuId, null);
}

/** Reset this tab (Menus): deletes `menus`. */
export function resetMenuOverrides(scope: ActionsScope): Promise<void> {
	return editActions(scope, (doc) => {
		delete doc.menus;
	});
}

function menusContaining(id: string): string[] {
	const model = getEffectiveModel();
	return model.menus.ids.filter((menuId) => {
		const menu = model.menus.get(menuId);
		return Boolean(menu?.items.some((item) => item.kind === 'action' && item.id === id));
	});
}

/**
 * Hides `id` from `menuIds` (default: every menu that contains it — D-06's
 * per-action "Hide"). Hiding is not unbinding (DEC-58): no key is touched.
 */
export function hideAction(scope: ActionsScope, id: string, menuIds?: string[]): Promise<void> {
	if (isLockedAction(id)) return Promise.reject(new LockedActionError(id));
	const targets = menuIds ?? menusContaining(id);
	if (targets.length === 0) return Promise.resolve();
	return editActions(scope, (doc) => {
		const menus = { ...(doc.menus ?? {}) };
		for (const menuId of targets) {
			const override: MenuOverride = { ...(menus[menuId] ?? {}) };
			const hidden = override.hidden ?? [];
			if (!hidden.includes(id)) override.hidden = [...hidden, id];
			menus[menuId] = override;
		}
		doc.menus = menus;
	});
}

/** Removes `id` from `hidden` at `scope` (of `menuIds`, default every menu). */
export function unhideAction(scope: ActionsScope, id: string, menuIds?: string[]): Promise<void> {
	return editActions(scope, (doc) => {
		if (!doc.menus) return;
		const menus = { ...doc.menus };
		for (const menuId of menuIds ?? Object.keys(menus)) {
			const override = menus[menuId];
			if (!override?.hidden?.includes(id)) continue;
			const hidden = override.hidden.filter((h) => h !== id);
			const next: MenuOverride = { ...override };
			if (hidden.length > 0) next.hidden = hidden;
			else delete next.hidden;
			menus[menuId] = next;
		}
		doc.menus = menus;
	});
}

export function addKeybinding(scope: ActionsScope, rule: KeybindingRule): Promise<void> {
	return editKeybindings(scope, (doc) => {
		doc.bindings = [...(doc.bindings ?? []), clone(rule)];
	});
}

/** The `when` a writer stores for an entry: omitted for `always` (§1.5). */
function storedWhen(when: string | undefined): string | undefined {
	try {
		const normalized = normalizeWhen(when);
		return normalized ? normalized : undefined;
	} catch {
		return when || undefined;
	}
}

/** The negative rule that removes exactly `entry` (§1.5). */
function negativeFor(entry: KeymapEntry): KeybindingRule {
	const when = storedWhen(entry.when);
	return {
		key: entry.key,
		command: `-${entry.command}`,
		...(when ? { when } : {}),
		...(entry.scope === 'os' ? { scope: 'os' as const } : {}),
		...(entry.platformOnly ? { platform: entry.platformOnly } : {}),
	};
}

function ownRule(scope: ActionsScope, entry: KeymapEntry, doc: KeybindingsDocument): number | null {
	if (entry.origin?.scope !== scope) return null;
	const rule = doc.bindings?.[entry.origin.index];
	return rule && rule.command === entry.command ? entry.origin.index : null;
}

/**
 * Rebinds `entry` to `newKey` at `scope` (the Keys tab). When `entry` is
 * that scope's own rule it is edited in place; otherwise one negative rule
 * for the old key and one positive rule for the new key are appended (§1.5).
 */
export function rebindKey(
	scope: ActionsScope,
	entry: KeymapEntry,
	newKey: string,
	opts?: { when?: string }
): Promise<void> {
	return editKeybindings(scope, (doc) => {
		const bindings = [...(doc.bindings ?? [])];
		const when = storedWhen(opts?.when ?? entry.when);
		const positive: KeybindingRule = {
			key: newKey,
			command: entry.command,
			...(when ? { when } : {}),
			...(entry.scope === 'os' ? { scope: 'os' as const } : {}),
			...(entry.platformOnly ? { platform: entry.platformOnly } : {}),
		};
		const own = ownRule(scope, entry, doc);
		if (own !== null) {
			bindings[own] = { ...bindings[own], ...positive };
			if (!when) delete bindings[own].when;
		} else {
			bindings.push(negativeFor(entry), positive);
		}
		doc.bindings = bindings;
	});
}

/** Unbinds `entry` at `scope`: removes that scope's own rule, else appends
 *  one negative rule. The key is never tombstoned (DEC-58). */
export function unbindKey(scope: ActionsScope, entry: KeymapEntry): Promise<void> {
	return editKeybindings(scope, (doc) => {
		const bindings = [...(doc.bindings ?? [])];
		const own = ownRule(scope, entry, doc);
		if (own !== null) bindings.splice(own, 1);
		else bindings.push(negativeFor(entry));
		doc.bindings = bindings;
	});
}

/** Reset: removes every rule (positive or negative) for `command` at
 *  `scope`; never writes the default back (WP-60 DoD). */
export function resetKeyOverride(scope: ActionsScope, command: string): Promise<void> {
	return editKeybindings(scope, (doc) => {
		if (!doc.bindings) return;
		doc.bindings = doc.bindings.filter((rule) => rule.command !== command && rule.command !== `-${command}`);
	});
}

export function removeKeybinding(scope: ActionsScope, index: number): Promise<void> {
	return editKeybindings(scope, (doc) => {
		if (!doc.bindings) return;
		doc.bindings = doc.bindings.filter((_, i) => i !== index);
	});
}

/** Reset this tab (Keys): deletes `bindings`. */
export function resetKeybindings(scope: ActionsScope): Promise<void> {
	return editKeybindings(scope, (doc) => {
		delete doc.bindings;
	});
}
