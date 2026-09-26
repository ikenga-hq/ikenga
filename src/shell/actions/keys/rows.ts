// D-06 Keys tab (WP-60) — pure data derivation over G-ACTIONS-API. Turns the
// effective keymap into the rows the table renders: every bound entry
// (`model.keymap.entries`, default < package < personal < project), every
// package key request that lost its grant (§7.4 — shown "requested `<key>` —
// held by `<command>`", rebindable like any action), and every project rule
// held until trust (DEC-65, §8.3). Conflict grouping is never string
// equality — it reads `model.keymap.conflicts[platform]`, which is already
// computed over normalized `when`s (WP-49/52).

import type {
	ActionsScope,
	EffectiveAction,
	EffectiveModel,
	HeldKeybinding,
	KeybindingRule,
	KeyHolder,
	NegativeRuleResult,
	PackageKeyRequest,
} from '@/lib/actions/store';
import type { KeymapConflictPair, KeymapConflicts, KeymapPlatform } from '@/lib/keymap/registry';
import { comparableKeySequence, entriesForPlatform, type EffectiveKeymapEntry } from '@/lib/keymap/registry';

export type KeyRowKind = 'bound' | 'requested' | 'held' | 'unbound';

/** One row of the Keys table. `kind` decides which fields are set:
 *  `bound` → `entry`; `requested` → `request` (+ `heldBy` when lost);
 *  `held` → `held` (DEC-65, never in `entries`); `unbound` → `negative`, a
 *  scope's own negative rule that actually removes something (§1.5) — the
 *  only way that rule is otherwise visible is the raw JSON file. */
export interface KeyRow {
	kind: KeyRowKind;
	rowId: string;
	command: string;
	label: string;
	action: EffectiveAction | undefined;
	/** Canonical key, or null for a held-request row (arrives unbound, §7.4). */
	key: string | null;
	when: string;
	source: string;
	osWide: boolean;
	entry?: EffectiveKeymapEntry;
	request?: PackageKeyRequest;
	heldBy?: KeyHolder;
	held?: HeldKeybinding;
	negative?: NegativeRuleResult;
}

function heldCommand(rule: KeybindingRule): string {
	return rule.command.startsWith('-') ? rule.command.slice(1) : rule.command;
}

/** Whether `row` is an override the given `scope` itself wrote (its own
 *  `keybindings.json`) — the only case a Reset is safe to show:
 *  `resetKeyOverride(scope, …)` (`@/lib/actions/store`) edits only that
 *  scope's own file, so gating on `row.source` alone (which layer wrote the
 *  *effective* entry, not which scope's file the UI is currently editing)
 *  could offer Reset for another scope's override and silently touch the
 *  wrong file (WP-60 review, HIGH). */
export function isScopeOverride(row: KeyRow, scope: ActionsScope): boolean {
	return row.kind === 'bound' && row.entry?.origin?.scope === scope;
}

/** Builds every row for `platform` — bound entries first (merge order),
 *  then package requests that never got their key, then `scope`'s own
 *  negative rules that remove something (`unbound`), then held project
 *  rules. `scope` is the tab's currently-edited scope (personal/project) —
 *  negatives are scoped to it (WP-60 review, MEDIUM) so a resettable
 *  "unbound here" row never mixes the other scope's file in. */
export function buildKeyRows(model: EffectiveModel, platform: KeymapPlatform, scope: ActionsScope): KeyRow[] {
	const rows: KeyRow[] = [];

	for (const entry of entriesForPlatform(model.keymap.entries, platform)) {
		const action = model.actionById.get(entry.command);
		rows.push({
			kind: 'bound',
			rowId: `bound:${entry.source}:${entry.command}:${entry.key}:${entry.scope ?? 'app'}:${entry.origin?.scope ?? ''}:${entry.origin?.index ?? ''}`,
			command: entry.command,
			label: action?.name ?? entry.label,
			action,
			key: entry.key,
			when: entry.when,
			source: entry.source,
			osWide: entry.scope === 'os',
			entry,
		});
	}

	for (const request of model.keymap.packageRequests) {
		const status = request.byPlatform[platform];
		// A granted request is already a `bound` row above (the package layer
		// entry `grantRequests` produced) — only a lost or invalid one gets its
		// own row here, per §7.4's "arrives unbound, rebindable" treatment.
		if (status.status === 'granted') continue;
		const action = model.actionById.get(request.actionId);
		rows.push({
			kind: 'requested',
			rowId: `requested:${request.actionId}:${platform}`,
			command: request.actionId,
			label: action?.name ?? request.actionId,
			action,
			key: null,
			when: request.when,
			source: 'package',
			osWide: false,
			request,
			heldBy: status.status === 'held' ? status.heldBy : undefined,
		});
	}

	// §1.5 "unbound here": a negative rule this scope wrote that actually
	// removes a survivor (`removed > 0` — a no-op negative is already
	// `W_NEGATIVE_NOOP` in `issues`, not worth its own row). Reset calls
	// `resetKeyOverride(scope, command)`, same as any other override.
	for (const negative of model.keymap.negatives) {
		if (negative.scope !== scope || negative.removed === 0) continue;
		const command = heldCommand(negative.rule);
		const action = model.actionById.get(command);
		rows.push({
			kind: 'unbound',
			rowId: `unbound:${negative.scope}:${negative.index}`,
			command,
			label: action?.name ?? command,
			action,
			key: negative.rule.key,
			when: negative.rule.when ?? 'always',
			source: negative.scope,
			osWide: negative.rule.scope === 'os',
			negative,
		});
	}

	for (const held of model.keymap.held) {
		const command = heldCommand(held.rule);
		const action = model.actionById.get(command);
		rows.push({
			kind: 'held',
			rowId: `held:${held.index}`,
			command,
			label: action?.name ?? command,
			action,
			key: held.rule.key,
			when: held.rule.when ?? 'always',
			source: 'project',
			osWide: false,
			held,
		});
	}

	return rows;
}

/** §6: the Keys tab's own "no focus condition" / "also in text fields"
 *  copy — display only, never fed back into a normalized comparison. */
export function whenLabel(when: string): string {
	if (when === 'always' || when === '') return 'always · also in text fields';
	if (when === '!inputFocus') return 'no focus condition';
	return when;
}

export function matchesQuery(row: KeyRow, query: string): boolean {
	if (!query.trim()) return true;
	const q = query.toLowerCase();
	const text = `${row.label} ${row.command} ${row.key ?? ''} ${row.when}`.toLowerCase();
	return text.includes(q);
}

/** "Search by pressed keys" mode: an exact match on the platform-resolved
 *  key, never a substring of the raw combo — a stroke recorded as `mod+k`
 *  should find every row bound to it however its own spelling round-trips. */
export function matchesKey(row: KeyRow, canonicalKey: string, platform: KeymapPlatform): boolean {
	if (!row.key) return false;
	try {
		return comparableKeySequence(row.key, platform) === comparableKeySequence(canonicalKey, platform);
	} catch {
		return false;
	}
}

export interface RowConflicts {
	clashes: KeymapConflictPair[];
	precedence: KeymapConflictPair[];
}

const EMPTY_CONFLICTS: RowConflicts = { clashes: [], precedence: [] };

/** DEC-59: never string equality — reads the pair list `conflicts()` already
 *  built over normalized `when`s, matched back to this row's own entry by
 *  object identity (the same `KeymapEntry` instances `conflicts()` read). */
export function conflictsForRow(row: KeyRow, conflicts: KeymapConflicts): RowConflicts {
	const entry = row.entry;
	if (!entry) return EMPTY_CONFLICTS;
	const clashes = conflicts.clashes.filter((p) => p.a === entry || p.b === entry);
	const precedence = conflicts.precedence.filter((p) => p.a === entry || p.b === entry);
	if (clashes.length === 0 && precedence.length === 0) return EMPTY_CONFLICTS;
	return { clashes, precedence };
}

export function otherEntry(pair: KeymapConflictPair, entry: EffectiveKeymapEntry): EffectiveKeymapEntry {
	return pair.a === entry ? pair.b : pair.a;
}
