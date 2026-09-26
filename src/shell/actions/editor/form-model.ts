// WP-58 — the Editor's form model: pure functions only (no React, no Tauri
// calls) so they're cheap to unit test. `index.tsx` holds the state and
// wires these to the shared store writes (`@/lib/actions/store`) and the
// runner (`@/lib/actions/runner`).
//
// Binds to G-ACTIONS (`plans/shell-ux-rearchitecture/drafts/actions-schema.md`,
// FROZEN Round 39): §1.2 (`actions.json` action fields), §1.3 (placements and
// the frozen menu-id table), §4 (the `when` grammar), §5 (the DEC-59 conflict
// rule), §8.1/§8.2 (run kinds and the six variables) and §11 (the D-06
// erratum: keys never live in `actions.json`, `action:<id>` is not a valid
// `command`, and D-06's `{at:"section",section:"Automations"}` preview is
// display-only — the file holds the full menu id).

import type {
	ActionRun,
	ActionRunKind,
	ChiTarget,
	KeybindingRule,
	Placement,
	UserAction,
} from '@/lib/actions/client';
import type {
	ActionsScope,
	EffectiveAction,
	EffectiveKeymapEntry,
	HeldKeybinding,
} from '@/lib/actions/store';
import { conflicts, type KeymapPlatform } from '@/lib/keymap/registry';
import { normalizeWhen } from '@/lib/keymap/when';

/** `EffectiveKeymapEntry` is `KeymapEntry` (`registry.ts`'s G-ACTIONS-API
 *  alias) — named locally so this file reads in keymap terms. */
type KeymapEntry = EffectiveKeymapEntry;

// ─── Placements (D-06's ten, G-ACTIONS §1.3) ────────────────────────────────

/** D-06's ten placement rows (`designs/actions.html` `PLACES`), kept in its
 *  order, mapped onto the real frozen menu ids (§1.3). `section` and `native`
 *  are parameterized (`section/<sectionId>`, `native/<top>`) — D-06 shows one
 *  checkbox for each and a picker for which one; this keeps that shape. */
export const PLACEMENT_IDS = [
	'palette',
	'files',
	'artifacts',
	'session',
	'tab',
	'pane',
	'section',
	'status',
	'rail',
	'native',
] as const;
export type PlacementId = (typeof PLACEMENT_IDS)[number];

export const PLACEMENT_LABELS: Readonly<Record<PlacementId, string>> = {
	palette: 'Command palette',
	files: 'Files context menu',
	artifacts: 'Artifacts context menu',
	session: 'Session context menu',
	tab: 'Tab context menu',
	pane: 'Pane ⋯ menu',
	section: 'Explorer section ⋯',
	status: 'Status bar',
	rail: 'Rail pin',
	native: 'Native menu bar',
};

/** Explorer sections that materialize a real `section/<id>` menu on 5b
 *  (§1.3's `section/<sectionId>` row) — offered as a picker rather than free
 *  text so a save can't silently target a section id that doesn't exist. */
export const SECTION_CHOICES: readonly { id: string; label: string }[] = [
	{ id: 'automations', label: 'Automations' },
	{ id: 'ngwa-project', label: 'Ngwa (project)' },
	{ id: 'files', label: 'Files' },
	{ id: 'artifacts', label: 'Artifacts' },
	{ id: 'session', label: 'Sessions' },
	{ id: 'scratchpads', label: 'Scratchpads' },
	{ id: 'todos', label: 'Todos' },
	{ id: 'views', label: 'Views' },
];

/** The `MENU_TREE` top menus a user action can sensibly join (§10.4) — the
 *  OS-role-only tops (`edit`, `window`) and `ikenga` (About/Settings/Quit)
 *  are left out of the picker. */
export const NATIVE_TOP_CHOICES: readonly { id: string; label: string }[] = [
	{ id: 'file', label: 'File' },
	{ id: 'view', label: 'View' },
	{ id: 'project', label: 'Project' },
	{ id: 'chi', label: 'Chi' },
	{ id: 'ngwa', label: 'Ngwa' },
	{ id: 'help', label: 'Help' },
];

export type PlacementFlags = Record<PlacementId, boolean>;

export function emptyPlacementFlags(): PlacementFlags {
	return {
		palette: false,
		files: false,
		artifacts: false,
		session: false,
		tab: false,
		pane: false,
		section: false,
		status: false,
		rail: false,
		native: false,
	};
}

/** A placement's real menu id (§1.3: `at` always carries the full id). */
export function menuIdForPlacement(id: PlacementId, sectionId: string, nativeTop: string): string {
	if (id === 'section') return `section/${sectionId || 'automations'}`;
	if (id === 'native') return `native/${nativeTop || 'file'}`;
	return id;
}

// ─── Run kinds (§8.1) ────────────────────────────────────────────────────────

export const RUN_TYPES: readonly { id: ActionRunKind; label: string }[] = [
	{ id: 'chi', label: 'Dispatch to Chi' },
	{ id: 'shell', label: 'Shell command' },
	{ id: 'iyke', label: 'iyke route' },
	{ id: 'skill', label: 'Skill' },
	{ id: 'workflow', label: 'Workflow' },
	{ id: 'open', label: 'Open view / URL' },
];

export interface EditorFormState {
	/** Empty only while creating brand new — derived live from `name` via
	 *  `slug()` until `idTouched`. There is no editable id field (D-06 shows
	 *  the id as a read-only caption, `actions.html:3077`), so a save is
	 *  always a new action or an upsert of the one it was opened for — never
	 *  a rename. */
	id: string;
	/** `true` once loaded from an existing action (`formFromAction`) — its id
	 *  then stays fixed regardless of further name edits (D-06 `_idLocked`). */
	idTouched: boolean;
	name: string;
	icon: string;
	description: string;

	runType: ActionRunKind;
	chiTarget: ChiTarget;
	chiEngineId: string;
	chiPrompt: string;
	shellCommand: string;
	shellCwd: string;
	shellConfirm: boolean;
	iykeRoute: string;
	iykeMethod: 'GET' | 'POST';
	skillName: string;
	workflowName: string;
	openUrl: string;

	placements: PlacementFlags;
	filesGlob: string;
	sectionId: string;
	nativeTop: string;
	/** Placements from the loaded action this editor can't fully model — an
	 *  id outside the frozen ten, or a `files` entry whose `when` isn't the
	 *  one glob shape it derives from `filesGlob` — carried through verbatim
	 *  on re-save instead of silently dropped (conformance §1.3/§8). */
	extraPlacements: Placement[];

	/** Canonical form (`@/lib/keymap/platform`), or `''` for unbound. */
	key: string;
	/** `false` until the user edits the When field directly — until then the
	 *  effective `when` is derived from the Files glob (§1.3's "the editor
	 *  derives both from its one glob field"). */
	whenTouched: boolean;
	whenValue: string;
}

export function slug(input: string): string {
	const s = input
		.toLowerCase()
		.trim()
		.replace(/[^a-z0-9]+/g, '-')
		.replace(/^-+|-+$/g, '');
	return s || 'untitled';
}

export function emptyForm(): EditorFormState {
	return {
		id: '',
		idTouched: false,
		name: '',
		icon: 'zap',
		description: '',
		runType: 'chi',
		chiTarget: 'active',
		chiEngineId: '',
		chiPrompt: '',
		shellCommand: '',
		shellCwd: '',
		shellConfirm: false,
		iykeRoute: '',
		iykeMethod: 'POST',
		skillName: '',
		workflowName: '',
		openUrl: '',
		placements: emptyPlacementFlags(),
		filesGlob: '',
		sectionId: 'automations',
		nativeTop: 'file',
		extraPlacements: [],
		key: '',
		whenTouched: false,
		whenValue: 'always',
	};
}

/** A menu id from an existing placement back onto the D-06 checkbox it
 *  toggles, plus the section/native picker value when parameterized. */
function applyPlacement(form: EditorFormState, at: string): void {
	if (at.startsWith('section/')) {
		form.placements.section = true;
		form.sectionId = at.slice('section/'.length) || form.sectionId;
		return;
	}
	if (at.startsWith('native/')) {
		form.placements.native = true;
		form.nativeTop = at.slice('native/'.length) || form.nativeTop;
		return;
	}
	if ((PLACEMENT_IDS as readonly string[]).includes(at)) {
		form.placements[at as PlacementId] = true;
	}
}

/** Whether `placement` is exactly the shape this editor can round-trip
 *  through its own fields — a bare `{ at }` for any known id, or `files`
 *  with a `when` of precisely `resource =~ '<glob>'` (nothing else, no
 *  extra clause) for the one glob field. Anything else — an id outside the
 *  frozen ten, or a `files` `when` this form can't rebuild byte-for-byte —
 *  is not, and is kept in `extraPlacements` instead of being reshaped (or
 *  silently dropped) by `buildPlacements`. */
const FILES_GLOB_WHEN = /^resource\s*=~\s*'([^']*)'$/;

function isRoundTrippablePlacement(placement: Placement): boolean {
	const known =
		(PLACEMENT_IDS as readonly string[]).includes(placement.at) ||
		placement.at.startsWith('section/') ||
		placement.at.startsWith('native/');
	if (!known) return false;
	const keys = Object.keys(placement);
	if (placement.at === 'files' && typeof placement.when === 'string') {
		return keys.length === 2 && FILES_GLOB_WHEN.test(placement.when);
	}
	return keys.length === 1 && keys[0] === 'at';
}

/** Loads an existing personal/project action (and its current key, if any)
 *  into a fresh form. `action.userAction` is the file's own record — the
 *  form edits a copy, never the live object. */
export function formFromAction(action: EffectiveAction, keyEntry: KeymapEntry | null): EditorFormState {
	const form = emptyForm();
	const run = action.userAction?.run ?? (action.run as ActionRun | undefined);
	form.id = action.id;
	form.idTouched = true; // an existing id is never re-derived from the name
	form.name = action.name;
	form.icon = action.icon || 'zap';
	form.description = action.description ?? '';

	if (run) {
		form.runType = run.kind;
		if (run.kind === 'chi') {
			form.chiTarget = run.target;
			form.chiEngineId = run.engineId ?? '';
			form.chiPrompt = run.prompt;
		} else if (run.kind === 'shell') {
			form.shellCommand = run.command;
			form.shellCwd = run.cwd ?? '';
			form.shellConfirm = run.confirm === true;
		} else if (run.kind === 'iyke') {
			form.iykeRoute = run.route;
			form.iykeMethod = run.method ?? 'POST';
		} else if (run.kind === 'skill') {
			form.skillName = run.skill;
		} else if (run.kind === 'workflow') {
			form.workflowName = run.workflow;
		} else if (run.kind === 'open') {
			form.openUrl = run.url;
		}
	}

	// A built-in's resolved placements carry `at: string`; the round-trip
	// check reads them through the stored `Placement` shape.
	const placements = (action.userAction?.placements ?? action.placements) as readonly Placement[];
	for (const placement of placements) {
		if (!isRoundTrippablePlacement(placement)) {
			form.extraPlacements.push(placement);
			continue;
		}
		applyPlacement(form, placement.at);
		if (placement.at === 'files' && typeof placement.when === 'string') {
			const m = placement.when.match(FILES_GLOB_WHEN);
			if (m) form.filesGlob = m[1];
		}
	}

	if (keyEntry) {
		form.key = keyEntry.key;
		form.whenTouched = true;
		form.whenValue = keyEntry.when || 'always';
	}

	return form;
}

// ─── Building the write payload ─────────────────────────────────────────────

export function buildRun(form: EditorFormState): ActionRun {
	switch (form.runType) {
		case 'chi':
			return {
				kind: 'chi',
				target: form.chiTarget,
				prompt: form.chiPrompt,
				...(form.chiTarget === 'engine' && form.chiEngineId.trim() ? { engineId: form.chiEngineId.trim() } : {}),
			};
		case 'shell':
			return {
				kind: 'shell',
				command: form.shellCommand,
				...(form.shellCwd.trim() ? { cwd: form.shellCwd.trim() } : {}),
				...(form.shellConfirm ? { confirm: true } : {}),
			};
		case 'iyke':
			return {
				kind: 'iyke',
				route: form.iykeRoute,
				...(form.iykeMethod !== 'POST' ? { method: form.iykeMethod } : {}),
			};
		case 'skill':
			return { kind: 'skill', skill: form.skillName };
		case 'workflow':
			return { kind: 'workflow', workflow: form.workflowName };
		case 'open':
			return { kind: 'open', url: form.openUrl };
		default:
			return { kind: 'open', url: '' };
	}
}

export function buildPlacements(form: EditorFormState): Placement[] {
	const out: Placement[] = [];
	for (const id of PLACEMENT_IDS) {
		if (!form.placements[id]) continue;
		const at = menuIdForPlacement(id, form.sectionId, form.nativeTop);
		if (id === 'files' && form.filesGlob.trim()) {
			out.push({ at, when: `resource =~ '${form.filesGlob.trim()}'` });
		} else {
			out.push({ at });
		}
	}
	// Whatever this form can't model — an unknown id, or a `files` `when`
	// that isn't the one glob shape — round-trips verbatim (§1.3/§8).
	out.push(...form.extraPlacements);
	return out;
}

export function buildUserAction(form: EditorFormState, scope: ActionsScope): UserAction {
	return {
		id: form.id,
		name: form.name,
		...(form.icon && form.icon !== 'zap' ? { icon: form.icon } : {}),
		...(form.description.trim() ? { description: form.description.trim() } : {}),
		run: buildRun(form),
		placements: buildPlacements(form),
		scope,
	};
}

/** §1.3: the key's `when` derived from the one glob field when nothing has
 *  overridden it — `filesFocus && resource =~ '<glob>'`, or `always` with no
 *  Files placement / no glob. */
export function derivedKeyWhen(form: EditorFormState): string {
	if (form.placements.files && form.filesGlob.trim()) {
		return `filesFocus && resource =~ '${form.filesGlob.trim()}'`;
	}
	return 'always';
}

export function effectiveKeyWhen(form: EditorFormState): string {
	if (!form.whenTouched) return derivedKeyWhen(form);
	return form.whenValue.trim() || 'always';
}

export function buildKeybindingRule(form: EditorFormState): KeybindingRule | null {
	if (!form.key) return null;
	const when = effectiveKeyWhen(form);
	return { key: form.key, command: form.id, ...(when !== 'always' ? { when } : {}) };
}

// ─── What Save actually writes to keybindings.json (§1.5, §11) ─────────────
//
// `rebindKey` / `unbindKey` (`@/lib/actions/store`) edit the reference
// entry's own rule in place when it's already `scope`'s, or otherwise append
// one negative rule for the old key plus one positive rule for the new one.
// `planKeybindingWrite` is the one place that works out which of those
// happens and what the stored `when` normalizes to (omitted for "always",
// same as `store.ts`'s own `storedWhen`) — `index.tsx`'s Save and
// `PreviewPane`'s "It writes" box both read it, so a save can never write
// something the preview didn't just show.

/** The `when` a writer stores for a rule: omitted for "always" (§1.5) —
 *  mirrors `store.ts`'s own `storedWhen`, which this file can't import
 *  (it's not part of the frozen G-ACTIONS-API surface), so the two are kept
 *  in sync by hand against the same contract section. */
function storedWhenFor(when: string | undefined): string | undefined {
	try {
		const normalized = normalizeWhen(when);
		return normalized ? normalized : undefined;
	} catch {
		return when || undefined;
	}
}

/** The negative rule that removes exactly `entry`, same shape as `store.ts`'s
 *  `negativeFor` (§1.5): the old key, `-command`, the entry's own `when` /
 *  `scope` / `platform` carried over untouched. */
function negativeRuleFor(entry: KeymapEntry): KeybindingRule {
	const when = storedWhenFor(entry.when);
	return {
		key: entry.key,
		command: `-${entry.command}`,
		...(when ? { when } : {}),
		...(entry.scope === 'os' ? { scope: 'os' as const } : {}),
		...(entry.platformOnly ? { platform: entry.platformOnly } : {}),
	};
}

export interface KeybindingWritePlan {
	/** What `index.tsx`'s Save should call: nothing, `addKeybinding`,
	 *  `rebindKey`, or `unbindKey`. */
	action: 'none' | 'add' | 'rebind' | 'unbind';
	/** The rule(s) that land in `keybindings.json` — zero (a same-scope
	 *  unbind, or nothing to do), one (a fresh bind, or an in-place edit),
	 *  or two (a cross-scope rebind/unbind: the negative for the old key,
	 *  then the positive for the new one, in write order, §1.5). */
	rules: KeybindingRule[];
}

/**
 * Predicts exactly what saving `form` at `scope` does to `keybindings.json`,
 * given the reference entry (`bindingsFor(form.id)[0]`, any scope) that
 * `rebindKey` / `unbindKey` would edit or displace.
 */
export function planKeybindingWrite(
	form: EditorFormState,
	referenceKeyEntry: KeymapEntry | null,
	scope: ActionsScope
): KeybindingWritePlan {
	const ownAtScope = referenceKeyEntry?.origin?.scope === scope;
	if (!form.key) {
		if (!referenceKeyEntry) return { action: 'none', rules: [] };
		if (ownAtScope) return { action: 'unbind', rules: [] };
		return { action: 'unbind', rules: [negativeRuleFor(referenceKeyEntry)] };
	}
	const when = storedWhenFor(effectiveKeyWhen(form));
	const rule: KeybindingRule = { key: form.key, command: form.id, ...(when ? { when } : {}) };
	if (!referenceKeyEntry) return { action: 'add', rules: [rule] };
	if (ownAtScope) return { action: 'rebind', rules: [rule] };
	return { action: 'rebind', rules: [negativeRuleFor(referenceKeyEntry), rule] };
}

// ─── Required-field validation (the server re-validates; this is only the
// immediate, in-form feedback so Save doesn't round-trip on the obvious) ────

export function runRequiresField(kind: ActionRunKind): string | null {
	switch (kind) {
		case 'chi':
			return 'a prompt';
		case 'shell':
			return 'a command';
		case 'iyke':
			return 'a route';
		case 'skill':
			return 'a skill name';
		case 'workflow':
			return 'a workflow id';
		case 'open':
			return 'a URL or view path';
		default:
			return null;
	}
}

export function missingRunField(form: EditorFormState): boolean {
	switch (form.runType) {
		case 'chi':
			return !form.chiPrompt.trim();
		case 'shell':
			return !form.shellCommand.trim();
		case 'iyke':
			return !form.iykeRoute.trim();
		case 'skill':
			return !form.skillName.trim();
		case 'workflow':
			return !form.workflowName.trim();
		case 'open':
			return !form.openUrl.trim();
		default:
			return false;
	}
}

// ─── The DEC-59 inline conflict card (§5) ───────────────────────────────────

export interface EditorConflict {
	/** The other command claiming the same platform-resolved key + the same
	 *  normalized `when` — never string equality (§5). */
	other: KeymapEntry;
}

/** DEC-65's held project rules (`model.keymap.held`) sit outside `entries`
 *  and outside `conflicts()` until the project is trusted — but the key they
 *  ask for is still spoken for: recorder that key today and the same clash
 *  appears the moment they're trusted. Negative rules (`-command`) claim
 *  nothing, so only the positive ones are turned into synthetic entries. */
function heldConflictEntries(held: readonly HeldKeybinding[]): KeymapEntry[] {
	return held
		.filter((h) => !h.rule.command.startsWith('-'))
		.map((h) => ({
			command: h.rule.command,
			key: h.rule.key,
			when: h.rule.when ?? 'always',
			source: 'project',
			label: h.rule.command,
		}));
}

/**
 * Whether saving `form`'s key (at `platform`) would clash with something
 * else in the effective keymap, per the one DEC-59 rule — reuses
 * `conflicts()` (`@/lib/keymap/registry`) rather than re-deriving the rule,
 * so this can never drift from what `conflicts()` reports elsewhere (the
 * Keys tab, `resolveKeypress`). `held` (DEC-65) is checked too, so a
 * project's not-yet-trusted rule for the same key still surfaces here.
 */
export function findEditorConflict(
	entries: readonly KeymapEntry[],
	actionId: string,
	form: EditorFormState,
	platform: KeymapPlatform,
	held: readonly HeldKeybinding[] = []
): EditorConflict | null {
	if (!form.key) return null;
	// Drop this action's own existing binding(s) first — comparing the draft
	// against its own prior self is not a conflict.
	const base = entries.filter((e) => e.command !== actionId);
	const draft: KeymapEntry = {
		command: actionId,
		key: form.key,
		when: effectiveKeyWhen(form),
		source: 'personal',
		label: form.name || actionId,
	};
	const result = conflicts({ entries: [...base, ...heldConflictEntries(held), draft], platform });
	const hit = result.clashes.find((pair) => pair.a.command === actionId || pair.b.command === actionId);
	if (!hit) return null;
	return { other: hit.a.command === actionId ? hit.b : hit.a };
}

/** The "Restrict to `<when>`" suggestion (§5's third resolution): narrows
 *  the entry being edited rather than touching the other one, so the pair
 *  stops being a clash (same key, now a different normalized `when`) and
 *  becomes non-conflicting or precedence instead. */
export function suggestedRestriction(form: EditorFormState): string {
	if (form.placements.files) {
		return `filesFocus && resource =~ '${form.filesGlob.trim() || '*.*'}'`;
	}
	return '!inputFocus';
}
