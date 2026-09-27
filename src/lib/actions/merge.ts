// The effective-model merge (WP-52) — pure functions, no I/O. `store.ts`
// feeds it the file layer (WP-50's `actions_read_files`) and the package
// projection (`pkg_kernel_status`), and publishes the result.
//
// G-ACTIONS sections implemented here:
// - §2.1 layers: default < package < personal < project; files load
//   personal, then project; within a layer, declaration order.
// - §2.2 effective keymap, per platform: (0) drop held project rules
//   (DEC-65, §8.3); (1) drop rules whose `platform` excludes this platform;
//   (2) resolve `mod`; (3) apply negative rules in merge order on the
//   platform-resolved key. A negative rule removes earlier positives for the
//   same command (and, with a `when`, the same normalized `when`) and never
//   tombstones the key (DEC-58).
// - §6 / DEC-60: `scope: "os"` rules only from default + personal (a project
//   OS rule is dropped here as well as refused by the validator).
// - §7.4 / DEC-54: a package key request is granted only if nothing holds
//   the key (any effective single-stroke default/personal/project rule
//   whatever its `when`, any chord prefix, any OS key, an earlier grant, a
//   predefined native-role accelerator); otherwise the action arrives
//   unbound. Grants are recomputed on every merge. The request's `when` is
//   derived from its `ContextSelector` (§7.3, G-71), or `!inputFocus` for a
//   `command_palette[]` shortcut (§12, G-70). Held project rules never hold
//   a key against a request (DEC-65).
// - §1.2: a project action with a personal action's id wins whole.
// - §1.4 / §9.2 / DEC-58 menus: `menus.ts`.
// - §1.6 model-level warnings the Rust validator defers to the merge
//   (WP-50 → WP-52 hand-off): `W_UNKNOWN_ICON` against the full Lucide list,
//   `W_UNKNOWN_COMMAND` for ids no source defines (menus `items` / `hidden`,
//   binding commands), `W_NEGATIVE_NOOP`; plus the merge's own
//   `E_LOCKED_HIDDEN` rejection.

import { DEFAULT_KEYMAP, type KeymapEntry, type KeymapSource } from '@/lib/keymap/defaults';
import { canonicalizeKeySequence, validateKeySequence } from '@/lib/keymap/platform';
import {
	comparableKeySequence,
	conflicts,
	entriesForPlatform,
	type KeymapConflicts,
	type KeymapPlatform,
} from '@/lib/keymap/registry';
import { normalizeWhen } from '@/lib/keymap/when';
import { buildMenu, type EffectiveMenu, type MenuMergeInput, type MenuMergeIssue, menuIdsFor } from './menus';
import {
	builtinActions,
	builtinToEffective,
	type EffectiveAction,
	isKnownIconName,
	type PackageActionSource,
	packageKeyWhen,
	packageToEffective,
	userToEffective,
} from './registry';
import type {
	ActionsFileKind,
	ActionsFilesResult,
	ActionsScope,
	KeybindingRule,
	MenuOverride,
	TrustState,
	UserAction,
	ValidationErrorCode,
	ValidationWarningCode,
} from './types';

export const PLATFORMS: readonly KeymapPlatform[] = ['mac', 'other'];

// §2.3 layer rank and keypress winner live beside `useKey` in the keymap
// registry (one resolver for the dispatcher, `useKey` and the Keys tab).
export { LAYER_RANK, resolveKeypressWinner } from '@/lib/keymap/registry';

/** §7.4 item 5: predefined native-role accelerators (`MENU_TREE`
 *  `predefined` items) — held on every platform, plus ⌘H / ⌘M on macOS. */
const NATIVE_ROLE_KEYS: Readonly<Record<KeymapPlatform, readonly string[]>> = {
	mac: ['mod+z', 'mod+shift+z', 'mod+x', 'mod+c', 'mod+v', 'mod+a', 'mod+q', 'mod+h', 'mod+m'],
	other: ['mod+z', 'mod+shift+z', 'mod+x', 'mod+c', 'mod+v', 'mod+a', 'mod+q'],
};

// ─── Result types ────────────────────────────────────────────────────────────

/** What holds a key against a package request (§7.4). */
export type KeyHolder =
	| { kind: 'binding'; command: string; layer: KeymapSource }
	| { kind: 'chord'; command: string; layer: KeymapSource }
	| { kind: 'os'; command: string; layer: KeymapSource }
	| { kind: 'package'; command: string }
	| { kind: 'native-role'; key: string };

export type KeyRequestPlatformStatus =
	| { status: 'granted' }
	| { status: 'held'; heldBy: KeyHolder }
	| { status: 'invalid'; reason: string };

/** A package action's DEC-54 key request and what became of it. A held
 *  request arrives unbound and is shown "requested `<key>` — held by
 *  `<command>`" in the Keys tab (§7.4), rebindable like any action. */
export interface PackageKeyRequest {
	actionId: string;
	pkgId: string;
	/** Canonical form of the requested key. */
	key: string;
	/** Derived `when` (§7.3 / §12). */
	when: string;
	origin: 'context_action' | 'command_palette';
	byPlatform: Record<KeymapPlatform, KeyRequestPlatformStatus>;
}

/** A project rule held out of the keymap until the project's keybindings
 *  are trusted (DEC-65, §8.3): it fires nothing, unbinds nothing, is not in
 *  `conflicts()` and holds no key. Listed "held until trusted". */
export interface HeldKeybinding {
	index: number;
	rule: KeybindingRule;
	/** The trust state that holds it (`untrusted`, `changed`, or `unknown`
	 *  when trust could not be read — fail closed). */
	trust: TrustState | 'unknown';
}

/** A negative rule (§1.5) and how many bindings it removed (every eligible
 *  platform counted). `removed === 0` is `W_NEGATIVE_NOOP`. */
export interface NegativeRuleResult {
	scope: ActionsScope;
	index: number;
	rule: KeybindingRule;
	removed: number;
}

export interface EffectiveKeymap {
	/** Merge order (default, package, personal, project). What `getKeymap()`
	 *  returns once published. `platformOnly` is narrowed where a rule
	 *  survives on one platform only. */
	entries: KeymapEntry[];
	/** DEC-65 — project rules held until trusted (empty when trusted). */
	held: HeldKeybinding[];
	/** Whether the project's keybindings are held at all. */
	projectHeld: boolean;
	/** Every package key request, in grant order. */
	packageRequests: PackageKeyRequest[];
	/** Every negative rule, personal then project, file order (held project
	 *  negatives are in `held`, not here). */
	negatives: NegativeRuleResult[];
	/** DEC-59 per platform, over `entries` (held rules excluded). */
	conflicts: Record<KeymapPlatform, KeymapConflicts>;
}

/** A problem the merge found, keyed to a file location. */
export interface ModelIssue {
	level: 'error' | 'warning';
	code: ValidationErrorCode | ValidationWarningCode;
	scope: ActionsScope;
	file: ActionsFileKind;
	/** JSON pointer into that file. */
	path: string;
	message: string;
}

export interface EffectiveMenus {
	/** Menu ids materialized in this model. */
	ids: string[];
	/** The effective menu for any menu id (`section/<id>` computed on demand
	 *  and memoized), or null for an unknown id with no contents. */
	get(menuId: string): EffectiveMenu | null;
}

export interface EffectiveModel {
	/** Every action in force: built-ins, packages (grant order), personal
	 *  (not overridden), project. */
	actions: EffectiveAction[];
	actionById: ReadonlyMap<string, EffectiveAction>;
	/** Personal actions whose id the project redefines (§1.2). */
	shadowedActions: EffectiveAction[];
	keymap: EffectiveKeymap;
	menus: EffectiveMenus;
	/** Model-level issues (the files' own validation stays on `files`). */
	issues: ModelIssue[];
	files: ActionsFilesResult | null;
	projectId: string | null;
	projectRoot: string | null;
}

export interface MergeInput {
	/** WP-50's `actions_read_files` result; null before the first read. */
	files: ActionsFilesResult | null;
	/** Package actions in grant order (`readPackageActions`). */
	packages: readonly PackageActionSource[];
	/** The default layer; `DEFAULT_KEYMAP` unless a test substitutes one. */
	defaults?: readonly KeymapEntry[];
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/** JSON-pointer segment escaping (RFC 6901). */
export function pointerSegment(segment: string | number): string {
	return String(segment).replace(/~/g, '~0').replace(/\//g, '~1');
}

function safeNormalize(when: string | undefined | null): string {
	try {
		return normalizeWhen(when);
	} catch {
		return `⟨invalid⟩ ${when ?? ''}`;
	}
}

/** Did the Rust validator already raise `code` at `path` for that file? */
function hasFileWarning(
	files: ActionsFilesResult | null,
	scope: ActionsScope,
	kind: ActionsFileKind,
	code: string,
	path: string
): boolean {
	const state = scope === 'personal' ? files?.personal : files?.project;
	const file = state ? (kind === 'actions' ? state.actions : state.keybindings) : null;
	return Boolean(file?.validation.warnings.some((w) => w.code === code && w.path === path));
}

/** §1.2: user actions in force, project over personal (whole, no field
 *  merge), plus the personal ones the project shadows. Personal first, then
 *  project, file order. (Same rule as `client.ts`'s `resolveUserActions`,
 *  kept here so the merge stays free of the Tauri client.) */
function resolveUser(files: ActionsFilesResult): Array<{
	action: UserAction;
	scope: ActionsScope;
	index: number;
	shadowed: boolean;
}> {
	const personal = files.personal.actions.document?.actions ?? [];
	const project = files.project?.actions.document?.actions ?? [];
	const projectIds = new Set(project.map((a) => a.id));
	return [
		...personal.map((action, index) => ({
			action,
			scope: 'personal' as const,
			index,
			shadowed: projectIds.has(action.id),
		})),
		...project.map((action, index) => ({ action, scope: 'project' as const, index, shadowed: false })),
	];
}

// ─── Keymap merge (§2.2, §7.4) ───────────────────────────────────────────────

interface RuleRec {
	/** Positive form (command without the `-`). */
	entry: KeymapEntry;
	negative: boolean;
	/** A negative rule's explicit `when` (normalized), or null = any. */
	negWhen: string | null;
	eligible: readonly KeymapPlatform[];
	scope?: ActionsScope;
	index?: number;
	rule?: KeybindingRule;
}

function eligibleFor(platform: unknown): readonly KeymapPlatform[] {
	return platform === 'mac' ? ['mac'] : platform === 'other' ? ['other'] : PLATFORMS;
}

function defaultRecs(defaults: readonly KeymapEntry[]): RuleRec[] {
	return defaults.map((entry) => ({
		entry,
		negative: false,
		negWhen: null,
		eligible: eligibleFor(entry.platformOnly),
	}));
}

function fileRecs(
	scope: ActionsScope,
	bindings: readonly KeybindingRule[],
	names: ReadonlyMap<string, string>
): RuleRec[] {
	const out: RuleRec[] = [];
	bindings.forEach((rule, index) => {
		if (!rule || typeof rule.key !== 'string' || typeof rule.command !== 'string') return;
		if (validateKeySequence(rule.key) !== null) return;
		const negative = rule.command.startsWith('-');
		const command = negative ? rule.command.slice(1) : rule.command;
		if (!command) return;
		const os = rule.scope === 'os';
		// DEC-60: a project OS rule is `E_OS_LAYER`; never in the keymap.
		if (os && scope === 'project') return;
		const when = typeof rule.when === 'string' && rule.when.trim() ? rule.when : undefined;
		const platformOnly = rule.platform === 'mac' || rule.platform === 'other' ? rule.platform : undefined;
		const entry: KeymapEntry = {
			command,
			key: canonicalizeKeySequence(rule.key),
			when: when ?? 'always',
			source: scope,
			label: names.get(command) ?? command,
			origin: { scope, index },
			...(os ? { scope: 'os' as const } : {}),
			...(platformOnly ? { platformOnly } : {}),
		};
		out.push({
			entry,
			negative,
			negWhen: negative && when ? safeNormalize(when) : null,
			eligible: eligibleFor(platformOnly),
			scope,
			index,
			rule,
		});
	});
	return out;
}

interface WalkResult {
	survivors: Set<RuleRec>;
	ordered: RuleRec[];
	removed: Map<RuleRec, number>;
}

function negativeMatches(neg: RuleRec, pos: RuleRec, platform: KeymapPlatform, negKey: string): boolean {
	if (pos.entry.command !== neg.entry.command) return false;
	if ((pos.entry.scope ?? 'app') !== (neg.entry.scope ?? 'app')) return false;
	if (comparableKeySequence(pos.entry.key, platform) !== negKey) return false;
	return neg.negWhen === null || safeNormalize(pos.entry.when) === neg.negWhen;
}

/** §2.2 steps 1–3 for one platform, over rules already in merge order. */
function walk(recs: readonly RuleRec[], platform: KeymapPlatform): WalkResult {
	let positives: RuleRec[] = [];
	const removed = new Map<RuleRec, number>();
	for (const rec of recs) {
		if (!rec.eligible.includes(platform)) continue;
		if (!rec.negative) {
			positives.push(rec);
			continue;
		}
		const negKey = comparableKeySequence(rec.entry.key, platform);
		const before = positives.length;
		positives = positives.filter((pos) => !negativeMatches(rec, pos, platform, negKey));
		removed.set(rec, (removed.get(rec) ?? 0) + (before - positives.length));
	}
	return { survivors: new Set(positives), ordered: positives, removed };
}

interface Holds {
	single: Map<string, RuleRec>;
	chordPrefix: Map<string, RuleRec>;
	nativeRoles: Set<string>;
}

function holdsFor(survivors: readonly RuleRec[], platform: KeymapPlatform): Holds {
	const holds: Holds = {
		single: new Map(),
		chordPrefix: new Map(),
		nativeRoles: new Set(NATIVE_ROLE_KEYS[platform].map((k) => comparableKeySequence(k, platform))),
	};
	for (const rec of survivors) {
		const strokes = comparableKeySequence(rec.entry.key, platform).split(' ');
		if (strokes.length === 2) {
			if (!holds.chordPrefix.has(strokes[0])) holds.chordPrefix.set(strokes[0], rec);
		} else if (!holds.single.has(strokes[0])) {
			holds.single.set(strokes[0], rec);
		}
	}
	return holds;
}

function grantRequests(
	packages: readonly PackageActionSource[],
	holdsByPlatform: Record<KeymapPlatform, Holds>
): { requests: PackageKeyRequest[]; grants: RuleRec[] } {
	const requests: PackageKeyRequest[] = [];
	const grants: RuleRec[] = [];
	const granted: Record<KeymapPlatform, Map<string, string>> = { mac: new Map(), other: new Map() };
	for (const pkgAction of packages) {
		const raw = pkgAction.keyRequest;
		if (typeof raw !== 'string' || raw.length === 0) continue;
		const when = packageKeyWhen(pkgAction);
		const grammar = validateKeySequence(raw);
		const chord = grammar === null && raw.trim().split(/\s+/).length > 1;
		const key = grammar === null ? canonicalizeKeySequence(raw) : raw;
		const byPlatform = {} as Record<KeymapPlatform, KeyRequestPlatformStatus>;
		const grantedOn: KeymapPlatform[] = [];
		for (const platform of PLATFORMS) {
			if (grammar !== null || chord) {
				byPlatform[platform] = {
					status: 'invalid',
					reason: chord ? 'a package key request is a single stroke (B-6)' : grammar ?? 'invalid key',
				};
				continue;
			}
			const k = comparableKeySequence(key, platform);
			const holds = holdsByPlatform[platform];
			const single = holds.single.get(k);
			const prefix = holds.chordPrefix.get(k);
			const earlier = granted[platform].get(k);
			let heldBy: KeyHolder | null = null;
			if (single) {
				heldBy =
					(single.entry.scope ?? 'app') === 'os'
						? { kind: 'os', command: single.entry.command, layer: single.entry.source }
						: { kind: 'binding', command: single.entry.command, layer: single.entry.source };
			} else if (prefix) {
				heldBy = { kind: 'chord', command: prefix.entry.command, layer: prefix.entry.source };
			} else if (earlier) {
				heldBy = { kind: 'package', command: earlier };
			} else if (holds.nativeRoles.has(k)) {
				heldBy = { kind: 'native-role', key };
			}
			if (heldBy) {
				byPlatform[platform] = { status: 'held', heldBy };
			} else {
				byPlatform[platform] = { status: 'granted' };
				granted[platform].set(k, pkgAction.id);
				grantedOn.push(platform);
			}
		}
		requests.push({
			actionId: pkgAction.id,
			pkgId: pkgAction.pkgId,
			key,
			when,
			origin: pkgAction.origin,
			byPlatform,
		});
		if (grantedOn.length > 0) {
			grants.push({
				entry: {
					command: pkgAction.id,
					key,
					when,
					source: 'package',
					label: pkgAction.name,
					pkgId: pkgAction.pkgId,
					...(grantedOn.length === 1 ? { platformOnly: grantedOn[0] } : {}),
				},
				negative: false,
				negWhen: null,
				eligible: grantedOn,
			});
		}
	}
	return { requests, grants };
}

function comparableOrNull(seq: string, platform: KeymapPlatform): string | null {
	try {
		return comparableKeySequence(seq, platform);
	} catch {
		return null;
	}
}

/**
 * What holds `key` on `platform` in an effective keymap (§7.4) — the same
 * notion the merge grants package requests against, read over `entries`
 * (the published keymap, package grants included): an effective single
 * stroke (default / personal / project, whatever its `when`; OS keys as
 * `os`), else a chord whose first stroke is `key`, else an earlier package
 * grant, else a predefined native-role accelerator. A chord `key` is held
 * only by an entry bound to the same full sequence. Null = free.
 */
export function keyHolderIn(
	entries: readonly KeymapEntry[],
	key: string,
	platform: KeymapPlatform
): KeyHolder | null {
	const k = comparableOrNull(key, platform);
	if (k === null) return null;
	const chord = k.includes(' ');
	let prefix: KeymapEntry | null = null;
	let pkg: KeymapEntry | null = null;
	for (const entry of entriesForPlatform(entries, platform)) {
		const seq = comparableOrNull(entry.key, platform);
		if (seq === null) continue;
		if (chord) {
			if (seq === k) return { kind: 'chord', command: entry.command, layer: entry.source };
			continue;
		}
		const strokes = seq.split(' ');
		if (strokes[0] !== k) continue;
		if (strokes.length > 1) {
			prefix ??= entry;
		} else if (entry.source === 'package') {
			pkg ??= entry;
		} else {
			return (entry.scope ?? 'app') === 'os'
				? { kind: 'os', command: entry.command, layer: entry.source }
				: { kind: 'binding', command: entry.command, layer: entry.source };
		}
	}
	if (chord) return null;
	if (prefix) return { kind: 'chord', command: prefix.command, layer: prefix.source };
	if (pkg) return { kind: 'package', command: pkg.command };
	const native = NATIVE_ROLE_KEYS[platform].some((n) => comparableOrNull(n, platform) === k);
	return native ? { kind: 'native-role', key } : null;
}

/** Whether the project's keybindings rules are held (DEC-65). Fail closed:
 *  only a `trusted` record releases a non-empty file. */
export function isProjectKeybindingsHeld(files: ActionsFilesResult | null): boolean {
	const bindings = files?.project?.keybindings.document?.bindings ?? [];
	if (bindings.length === 0) return false;
	const trust = files?.projectKeybindingsTrust ?? null;
	return trust?.state !== 'trusted';
}

export interface KeymapMergeResult extends EffectiveKeymap {
	/** Model issues found by the keymap merge. */
	issues: ModelIssue[];
}

/**
 * The effective keymap (§2.2 + §7.4). Pass A computes the non-package
 * effective rules to decide which keys are held; package requests are then
 * granted in grant order; pass B re-walks every layer (so a personal
 * negative rule can also remove a granted package key) and yields the
 * result.
 */
export function mergeKeymap(
	input: MergeInput,
	names: ReadonlyMap<string, string>,
	knownCommand: (id: string) => boolean
): KeymapMergeResult {
	const files = input.files;
	const defaults = defaultRecs(input.defaults ?? DEFAULT_KEYMAP);
	const personalBindings = files?.personal.keybindings.document?.bindings ?? [];
	const projectBindings = files?.project?.keybindings.document?.bindings ?? [];
	const projectHeld = isProjectKeybindingsHeld(files);
	const personal = fileRecs('personal', personalBindings, names);
	const project = projectHeld ? [] : fileRecs('project', projectBindings, names);

	// Pass A — holds (§7.4 items 1–3), held project rules excluded (DEC-65).
	const passA = [...defaults, ...personal, ...project];
	const holdsByPlatform = {} as Record<KeymapPlatform, Holds>;
	for (const platform of PLATFORMS) holdsByPlatform[platform] = holdsFor(walk(passA, platform).ordered, platform);

	const { requests, grants } = grantRequests(input.packages, holdsByPlatform);

	// Pass B — every layer, merge order.
	const all = [...defaults, ...grants, ...personal, ...project];
	const walks = {} as Record<KeymapPlatform, WalkResult>;
	for (const platform of PLATFORMS) walks[platform] = walk(all, platform);

	const entries: KeymapEntry[] = [];
	for (const rec of all) {
		if (rec.negative) continue;
		const on = rec.eligible.filter((p) => walks[p].survivors.has(rec));
		if (on.length === 0) continue;
		entries.push(on.length === rec.eligible.length ? rec.entry : { ...rec.entry, platformOnly: on[0] });
	}

	const issues: ModelIssue[] = [];
	const negatives: NegativeRuleResult[] = [];
	for (const rec of [...personal, ...project]) {
		if (!rec.negative || !rec.scope || rec.index === undefined || !rec.rule) continue;
		const removed = PLATFORMS.reduce((n, p) => n + (walks[p].removed.get(rec) ?? 0), 0);
		negatives.push({ scope: rec.scope, index: rec.index, rule: rec.rule, removed });
		if (removed === 0) {
			issues.push({
				level: 'warning',
				code: 'W_NEGATIVE_NOOP',
				scope: rec.scope,
				file: 'keybindings',
				path: `/bindings/${rec.index}`,
				message: `\`${rec.rule.command}\` on \`${rec.rule.key}\` removes no binding (inert)`,
			});
		}
	}

	// W_UNKNOWN_COMMAND for binding commands no source defines (both files;
	// held rules too — they are still in the file).
	const fileRules: Array<[ActionsScope, readonly KeybindingRule[]]> = [
		['personal', personalBindings],
		['project', projectBindings],
	];
	for (const [scope, bindings] of fileRules) {
		bindings.forEach((rule, index) => {
			if (!rule || typeof rule.command !== 'string') return;
			const command = rule.command.startsWith('-') ? rule.command.slice(1) : rule.command;
			if (!command || knownCommand(command)) return;
			const path = `/bindings/${index}`;
			if (hasFileWarning(files, scope, 'keybindings', 'W_UNKNOWN_COMMAND', path)) return;
			issues.push({
				level: 'warning',
				code: 'W_UNKNOWN_COMMAND',
				scope,
				file: 'keybindings',
				path,
				message: `no action defines \`${command}\` (rule kept, inert)`,
			});
		});
	}

	const trustState = files?.projectKeybindingsTrust?.state ?? 'unknown';
	const held: HeldKeybinding[] = projectHeld
		? projectBindings.map((rule, index) => ({ index, rule, trust: trustState }))
		: [];

	return {
		entries,
		held,
		projectHeld,
		packageRequests: requests,
		negatives,
		conflicts: {
			mac: conflicts({ entries, platform: 'mac' }),
			other: conflicts({ entries, platform: 'other' }),
		},
		issues,
	};
}

// ─── Actions (§1.2) ──────────────────────────────────────────────────────────

interface ActionsMergeResult {
	actions: EffectiveAction[];
	actionById: Map<string, EffectiveAction>;
	shadowed: EffectiveAction[];
	packageActions: EffectiveAction[];
	userActions: Record<ActionsScope, EffectiveAction[]>;
	issues: ModelIssue[];
}


export function mergeActions(input: MergeInput): ActionsMergeResult {
	const actions: EffectiveAction[] = [];
	const actionById = new Map<string, EffectiveAction>();
	const issues: ModelIssue[] = [];
	const add = (action: EffectiveAction) => {
		if (actionById.has(action.id)) return false;
		actions.push(action);
		actionById.set(action.id, action);
		return true;
	};

	for (const def of builtinActions()) add(builtinToEffective(def));
	const packageActions: EffectiveAction[] = [];
	for (const src of input.packages) {
		const action = packageToEffective(src);
		if (add(action)) packageActions.push(action);
	}

	const userActions: Record<ActionsScope, EffectiveAction[]> = { personal: [], project: [] };
	const shadowed: EffectiveAction[] = [];
	if (input.files) {
		for (const resolved of resolveUser(input.files)) {
			const effective = userToEffective(resolved.action, resolved.scope, resolved.index);
			if (resolved.shadowed) {
				shadowed.push({ ...effective, overriddenBy: 'project' });
				continue;
			}
			// A user id never shadows a built-in or package id (§10.1; the
			// validator refuses it as `E_ID_BUILTIN`).
			if (add(effective)) userActions[resolved.scope].push(effective);
		}
		// W_UNKNOWN_ICON against the full Lucide list (the Rust validator only
		// checks the kebab-case shape).
		for (const scope of ['personal', 'project'] as const) {
			const list = (scope === 'personal' ? input.files.personal : input.files.project)?.actions.document?.actions ?? [];
			list.forEach((action, index) => {
				if (typeof action.icon !== 'string' || isKnownIconName(action.icon)) return;
				const path = `/actions/${index}/icon`;
				if (hasFileWarning(input.files, scope, 'actions', 'W_UNKNOWN_ICON', path)) return;
				issues.push({
					level: 'warning',
					code: 'W_UNKNOWN_ICON',
					scope,
					file: 'actions',
					path,
					message: `\`${action.icon}\` is not a Lucide icon name (kept; renders \`zap\`)`,
				});
			});
		}
	}
	return { actions, actionById, shadowed, packageActions, userActions, issues };
}

// ─── The whole model ─────────────────────────────────────────────────────────

function menuIssueToModel(issue: MenuMergeIssue): ModelIssue {
	return {
		level: issue.code === 'E_LOCKED_HIDDEN' ? 'error' : 'warning',
		code: issue.code,
		scope: issue.scope,
		file: 'actions',
		path: `/menus/${pointerSegment(issue.menuId)}/${issue.field}/${issue.index}`,
		message: issue.message,
	};
}

/** Builds the effective model from the file layer and the package layer. */
export function buildEffectiveModel(input: MergeInput): EffectiveModel {
	const merged = mergeActions(input);
	const names = new Map<string, string>();
	for (const action of merged.actions) names.set(action.id, action.name);
	const keymap = mergeKeymap(input, names, (id) => merged.actionById.has(id));

	const overrides: MenuMergeInput['overrides'] = {
		personal: input.files?.personal.actions.document?.menus as Record<string, MenuOverride> | undefined,
		// DEC-65: project menu overrides apply before trust.
		project: input.files?.project?.actions.document?.menus as Record<string, MenuOverride> | undefined,
	};
	const base: Omit<MenuMergeInput, 'menuId'> = {
		actions: merged.actionById,
		packageActions: merged.packageActions,
		userActions: merged.userActions,
		overrides,
	};
	const menuIssues: MenuMergeIssue[] = [];
	const cache = new Map<string, EffectiveMenu | null>();
	const ids = menuIdsFor(base);
	for (const id of ids) cache.set(id, buildMenu({ ...base, menuId: id }, menuIssues));
	const menus: EffectiveMenus = {
		ids,
		get(menuId: string) {
			if (cache.has(menuId)) return cache.get(menuId) ?? null;
			// Built on demand (e.g. a `section/<id>` nothing names yet); its
			// issues were already reported if an override named it.
			const menu = buildMenu({ ...base, menuId }, []);
			cache.set(menuId, menu);
			return menu;
		},
	};

	// Unknown-id warnings the Rust validator already raised are not repeated.
	const menuModelIssues = menuIssues
		.map(menuIssueToModel)
		.filter((issue) => issue.level === 'error' || !hasFileWarning(input.files, issue.scope, 'actions', issue.code, issue.path));

	return {
		actions: merged.actions,
		actionById: merged.actionById,
		shadowedActions: merged.shadowed,
		keymap: {
			entries: keymap.entries,
			held: keymap.held,
			projectHeld: keymap.projectHeld,
			packageRequests: keymap.packageRequests,
			negatives: keymap.negatives,
			conflicts: keymap.conflicts,
		},
		menus,
		issues: [...merged.issues, ...keymap.issues, ...menuModelIssues],
		files: input.files,
		projectId: input.files?.projectId ?? null,
		projectRoot: input.files?.projectRoot ?? null,
	};
}
