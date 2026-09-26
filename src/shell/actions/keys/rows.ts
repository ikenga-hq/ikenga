// D-06 Keys tab (WP-60) — pure data derivation over G-ACTIONS-API. Turns the
// effective keymap into the rows the table renders: every bound entry
// (`model.keymap.entries`, default < package < personal < project), every
// package key request that lost its grant (§7.4 — shown "requested `<key>` —
// held by `<command>`", rebindable like any action), and every project rule
// held until trust (DEC-65, §8.3). Conflict grouping is never string
// equality — it reads `model.keymap.conflicts[platform]`, which is already
// computed over normalized `when`s (WP-49/52).

import type { EffectiveAction, EffectiveModel, HeldKeybinding, KeyHolder, PackageKeyRequest } from '@/lib/actions/store';
import type { KeymapConflictPair, KeymapConflicts, KeymapPlatform } from '@/lib/keymap/registry';
import { comparableKeySequence, entriesForPlatform, type EffectiveKeymapEntry } from '@/lib/keymap/registry';

export type KeyRowKind = 'bound' | 'requested' | 'held';

/** One row of the Keys table. `kind` decides which fields are set:
 *  `bound` → `entry`; `requested` → `request` (+ `heldBy` when lost);
 *  `held` → `held` (DEC-65, never in `entries`). */
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
}

function heldCommand(rule: HeldKeybinding['rule']): string {
	return rule.command.startsWith('-') ? rule.command.slice(1) : rule.command;
}

/** Builds every row for `platform` — bound entries first (merge order),
 *  then package requests that never got their key, then held project rules. */
export function buildKeyRows(model: EffectiveModel, platform: KeymapPlatform): KeyRow[] {
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
