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
//   type EffectiveKeymapEntry = KeymapEntry     (an entry of `getKeymap()`)
//   resolveKeypress(input: KeyboardEvent | { key: string }, ctx?: ContextKeys,
//       platform?: 'mac' | 'other', entries?: EffectiveKeymapEntry[])
//       → { winner: EffectiveKeymapEntry | null; candidates: EffectiveKeymapEntry[] }
//       THE "winner for a key + context" query (§2.3, DEC-58): candidates =
//       effective `scope:'app'` entries on the platform whose key matches
//       (an event: single strokes, IME / Dead keys match nothing; a string:
//       platform-resolved comparison, chords allowed), not hosted (§4.6),
//       `when` true against `ctx` (default: live context of the event
//       target); winner by layer, then `when` specificity, then merge order.
//       `useKey()` fires only its command's win. The dispatcher (WP-54), the
//       Keys tab and iyke use it. `entries` defaults to `getKeymap()`.
//       Lives in `@/lib/keymap/registry`; type `KeypressResolution`.
//   resolveKeypressWinner(candidates, keymap) — the §2.3 ranking alone
//   bindingsFor(actionId, platform?): EffectiveKeymapEntry[]   every
//       effective binding of the action (app + OS scope), merge order
//   keyHolder(key, platform?): KeyHolder | null   §7.4 "holds" over the
//       current model (package grants included): single stroke
//       (`binding` | `os`), else a chord prefix (`chord`), else an earlier
//       grant (`package`), else a native-role accelerator; null = free.
//       A chord key is held only by the same full sequence. WP-61's import.
//   `platform` defaults to the live platform everywhere.
//
// ── Lifecycle and change subscription ───────────────────────────────────────
//   startActionsStore(): Promise<EffectiveModel>   idempotent; first read +
//       subscriptions: `actions://changed` (watcher + trust grants/revokes),
//       `pkg-installed` / `pkg-uninstalled` / `pkg-reloaded`, and the active
//       project (a project change re-reads files AND packages, for that
//       project id). Every one re-merges without a restart. The `use*`
//       hooks call it too, but nothing else does: WP-54 calls it once at
//       the app root on boot (so `getKeymap()` / `useKey()` see the
//       effective keymap before any hook mounts).
//   stopActionsStore(): void                       tears down; getKeymap()
//       reverts to the defaults
//   refreshActionsModel(projectId?: string | null): Promise<EffectiveModel>
//       re-read files + packages (null = the Rust side resolves the project)
//   subscribeEffectiveModel(listener: (model) => void): () => void
//   Hand-off (WP-62): `use-iyke-shell-sync.ts` pushes the keymap to iyke
//   once; WP-62 must re-send it on every `subscribeKeymap()` notification.
//
// ── Writes ──────────────────────────────────────────────────────────────────
//   Every write goes through the WP-50 validator; a refused write throws
//   `ActionsValidationError` (re-exported here) and leaves the file untouched.
//   A file present on disk but not in force as written — stale, unreadable,
//   `document: null`, or any `validation.errors` — is never rewritten
//   (§1.1): `ActionsFileNotWritableError`, nothing written. Writes are
//   SERIALIZED: one module-level chain, call order; each edit re-reads the
//   files fresh (`readActionsFiles`, not the cached model), applies itself,
//   writes, re-merges — so overlapping edits and on-disk edits inside the
//   watcher debounce are never lost. Write-side types re-exported here:
//   `ActionsScope`, `KeybindingRule`, `MenuOverride`, `UserAction`.
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
//       entry's own rule at `scope` → edited in place (key + `when` only;
//       its `platform` / `scope` stay as written); otherwise one negative
//       rule for the old key + one positive rule for the new key (§1.5)
//   unbindKey(scope, entry: KeymapEntry): Promise<void>
//       own rule at `scope` → removed; otherwise one negative rule
//   Limitation: rebind / unbind at a scope BELOW the entry's layer (a
//   project rule at `personal`) would never win (§2.3) — both reject with
//   `LowerScopeOverrideError` ({scope, entry}) and write nothing.
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
import { isMacPlatform } from '@/lib/keymap/platform';
import {
	type EffectiveKeymapEntry,
	entriesForPlatform,
	type KeymapPlatform,
	LAYER_RANK,
	setEffectiveKeymap,
} from '@/lib/keymap/registry';
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
import { buildEffectiveModel, type EffectiveKeymap, type EffectiveModel, type KeyHolder, keyHolderIn } from './merge';
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
export type { EffectiveKeymapEntry, KeypressResolution } from '@/lib/keymap/registry';
export { resolveKeypress } from '@/lib/keymap/registry';
export { ActionsValidationError } from './client';
export type { ActionsScope, KeybindingRule, MenuOverride, UserAction } from './client';
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

/** A rebind / unbind at a scope below the layer the binding comes from
 *  (a project rule edited at `personal`): the lower layer's rule could never
 *  win (§2.3), so nothing is written. Edit it at `entry.source`'s scope. */
export class LowerScopeOverrideError extends Error {
	readonly scope: ActionsScope;
	readonly entry: KeymapEntry;
	constructor(scope: ActionsScope, entry: KeymapEntry) {
		super(
			`\`${entry.command}\` on \`${entry.key}\` comes from the ${entry.source} layer; a ${scope} rule cannot override it`
		);
		this.name = 'LowerScopeOverrideError';
		this.scope = scope;
		this.entry = entry;
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

/** `projectId` null = the Rust side resolves the active project. */
async function loadFiles(projectId: string | null = null): Promise<boolean> {
	const seq = ++filesSeq;
	const files = await readActionsFiles(projectId);
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

/** Re-reads the files and the package snapshot, then re-merges. `projectId`
 *  (default: resolved by the Rust side) names the project to read. */
export async function refreshActionsModel(projectId: string | null = null): Promise<EffectiveModel> {
	try {
		await Promise.all([loadFiles(projectId), loadPackages()]);
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
		// Project-scoped packages change with the project too: full refresh.
		if (a?.id !== b?.id || a?.root_path !== b?.root_path) void refreshActionsModel(a?.id ?? null);
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

function livePlatform(): KeymapPlatform {
	return isMacPlatform() ? 'mac' : 'other';
}

/** Every effective binding of `actionId` on `platform` (default: live), in
 *  merge order — in-app and OS scope, hosted commands included. */
export function bindingsFor(actionId: string, platform?: KeymapPlatform): EffectiveKeymapEntry[] {
	return entriesForPlatform(getEffectiveKeymap().entries, platform ?? livePlatform()).filter(
		(entry) => entry.command === actionId
	);
}

/** What holds `key` on `platform` (default: live) in the current model —
 *  the §7.4 "holds" notion package requests are granted against (null =
 *  free). See `keyHolderIn` (`merge.ts`). */
export function keyHolder(key: string, platform?: KeymapPlatform): KeyHolder | null {
	return keyHolderIn(getEffectiveKeymap().entries, key, platform ?? livePlatform());
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
	files: ActionsFilesResult,
	scope: ActionsScope,
	pick: (files: ActionsScopeFiles) => ActionsFileState<D>
): ActionsFileState<D> {
	const scoped = scope === 'personal' ? files.personal : files.project;
	if (!scoped) throw new ActionsFileNotWritableError(scope, 'the active project has no filesystem root');
	const state = pick(scoped);
	// §1.1: a file that is on disk but not in force as written (malformed,
	// unreadable, or failing validation — with or without a valid copy from
	// earlier this session) is never rewritten from the model.
	if (
		state.stale ||
		state.error ||
		(state.present && (state.document === null || state.validation.errors.length > 0))
	) {
		throw new ActionsFileNotWritableError(
			scope,
			`${state.path} is not in force as written (${state.error ?? 'invalid on disk'}); fix or reset the file first`
		);
	}
	return state;
}

/** Every write runs through this one chain, in call order: each edit reads
 *  the files fresh, applies itself and writes before the next one starts,
 *  so overlapping edits (or an on-disk edit inside the watcher's debounce)
 *  never overwrite each other. A failed write does not block the next. */
let writeChain: Promise<unknown> = Promise.resolve();

function serialized<T>(task: () => Promise<T>): Promise<T> {
	const run = writeChain.then(task, task);
	writeChain = run.catch(() => {});
	return run;
}

async function readForWrite(): Promise<ActionsFilesResult> {
	const model = getEffectiveModel();
	return readActionsFiles(model.files ? model.projectId : null);
}

function editActions(scope: ActionsScope, edit: (doc: ActionsDocument) => void): Promise<void> {
	return serialized(async () => {
		const files = await readForWrite();
		const state = writableState(files, scope, (f) => f.actions);
		const doc: ActionsDocument = state.document
			? clone(state.document)
			: { $schema: ACTIONS_SCHEMA, version: 1 };
		edit(doc);
		await writeActionsFile(scope, doc, files.projectId);
		await refreshFiles();
	});
}

function editKeybindings(scope: ActionsScope, edit: (doc: KeybindingsDocument) => void): Promise<void> {
	return serialized(async () => {
		const files = await readForWrite();
		const state = writableState(files, scope, (f) => f.keybindings);
		const doc: KeybindingsDocument = state.document
			? clone(state.document)
			: { $schema: KEYBINDINGS_SCHEMA, version: 1 };
		edit(doc);
		await writeKeybindingsFile(scope, doc, files.projectId);
		await refreshFiles();
	});
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

/** `entry` comes from a layer above `scope` (§2.3: that rule would win). */
function isLowerScope(scope: ActionsScope, entry: KeymapEntry): boolean {
	return LAYER_RANK[entry.source] > LAYER_RANK[scope];
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
	if (isLowerScope(scope, entry)) return Promise.reject(new LowerScopeOverrideError(scope, entry));
	return editKeybindings(scope, (doc) => {
		const bindings = [...(doc.bindings ?? [])];
		const when = storedWhen(opts?.when ?? entry.when);
		const own = ownRule(scope, entry, doc);
		if (own !== null) {
			// In place: only key and `when` change; the rule's own `platform`
			// and `scope` stay as the user wrote them.
			const next: KeybindingRule = { ...bindings[own], key: newKey };
			if (when) next.when = when;
			else delete next.when;
			bindings[own] = next;
		} else {
			bindings.push(negativeFor(entry), {
				key: newKey,
				command: entry.command,
				...(when ? { when } : {}),
				...(entry.scope === 'os' ? { scope: 'os' as const } : {}),
				...(entry.platformOnly ? { platform: entry.platformOnly } : {}),
			});
		}
		doc.bindings = bindings;
	});
}

/** Unbinds `entry` at `scope`: removes that scope's own rule, else appends
 *  one negative rule. The key is never tombstoned (DEC-58). */
export function unbindKey(scope: ActionsScope, entry: KeymapEntry): Promise<void> {
	if (isLowerScope(scope, entry)) return Promise.reject(new LowerScopeOverrideError(scope, entry));
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
