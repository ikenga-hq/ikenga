// WP-61 — "From a teammate's project file" import source (G-ACTIONS §8.3,
// DEC-55; B-26 / DEC-65). Reads another checkout's `<root>/.ikenga/actions.json`
// (and, best-effort, its sibling `keybindings.json`) from disk and diffs both
// against the *current* project's effective model. Both files are strict
// JSON (§1.1) — no JSONC tolerance here, unlike `vscode.ts`, because these
// are Ikenga's own file shapes, not a foreign tool's export.
//
// Everything this writes lands at **project** scope in the *active* project
// (there is no "pick a destination project" — the whole point is bringing a
// teammate's file into the one you have open): new actions go through the
// ordinary DEC-55 trust gate at run time (nothing extra to write — an id
// missing from the trust record is untrusted by construction, §8.3 "fail
// closed"), and new keybindings ride the same write path as any other edit,
// so B-26's hash-mismatch-holds-them-again mechanism applies for free the
// moment `bindings` changes on disk.

import { fsRead } from '@/lib/tauri-cmd';
import {
	addKeybinding,
	keyHolder,
	saveUserAction,
	type EffectiveModel,
	type KeybindingRule,
	type UserAction,
} from '@/lib/actions/store';
import type { ImportDiffRow } from './vscode-map';
import { resolveImportIcon } from './vscode-map';

/** DEC-55, G-ACTIONS §8.3: project-scope actions of these kinds refuse to
 *  run until the project is trusted. Mirrors `types.ts`'s `GATED_RUN_KINDS`
 *  — a small, frozen, schema-owned list (WP-61 imports types only from
 *  `@/lib/actions/store`, which doesn't re-export it; duplicating four
 *  literal strings from a frozen schema is cheaper than a second import
 *  seam for this one constant). */
const GATED_RUN_KINDS = new Set(['shell', 'iyke', 'skill', 'workflow']);

function decode(bytes: number[]): string {
	return new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(bytes));
}

export interface TeammateProjectSource {
	actionsPath: string;
	actions: UserAction[];
	bindings: KeybindingRule[];
	/** Null when no sibling `keybindings.json` could be read (fine — it's
	 *  optional, same as any `.ikenga/` scope, §1.1). */
	bindingsPath: string | null;
	bindingsReadError: string | null;
}

function siblingKeybindingsPath(actionsPath: string): string | null {
	if (!/actions\.json$/.test(actionsPath)) return null;
	return actionsPath.replace(/actions\.json$/, 'keybindings.json');
}

/** Loose shape check — real validation is the WP-50 Rust validator's job at
 *  write time (`saveUserAction` → `writeActionsFile`, which refuses a
 *  document with any `E_*` and leaves the file untouched); this only filters
 *  out rows too broken to even attempt. */
function looksLikeUserAction(value: unknown): value is UserAction {
	if (!value || typeof value !== 'object') return false;
	const v = value as Record<string, unknown>;
	return typeof v.id === 'string' && v.id.length > 0 && typeof v.name === 'string' && !!v.run && typeof v.run === 'object';
}

function looksLikeKeybindingRule(value: unknown): value is KeybindingRule {
	if (!value || typeof value !== 'object') return false;
	const v = value as Record<string, unknown>;
	return typeof v.key === 'string' && typeof v.command === 'string';
}

/** Reads a teammate's `actions.json` (required) and its sibling
 *  `keybindings.json` (best-effort). Throws only when `actionsPath` itself
 *  can't be read or parsed — a failed sibling read is reported in the
 *  result, not thrown, since a project can have actions with no
 *  keybindings file at all. */
export async function readTeammateProjectFile(actionsPath: string): Promise<TeammateProjectSource> {
	const raw = decode((await fsRead(actionsPath)).bytes);
	let parsed: unknown;
	try {
		parsed = JSON.parse(raw);
	} catch (err) {
		throw new Error(`not valid JSON: ${err instanceof Error ? err.message : String(err)}`);
	}
	const doc = parsed as { actions?: unknown } | null;
	const actions = Array.isArray(doc?.actions) ? doc.actions.filter(looksLikeUserAction) : [];

	const bindingsPath = siblingKeybindingsPath(actionsPath);
	let bindings: KeybindingRule[] = [];
	let bindingsReadError: string | null = null;
	let readBindingsPath: string | null = null;
	if (bindingsPath) {
		try {
			const bindingsRaw = decode((await fsRead(bindingsPath)).bytes);
			const bindingsDoc = JSON.parse(bindingsRaw) as { bindings?: unknown } | null;
			bindings = Array.isArray(bindingsDoc?.bindings) ? bindingsDoc.bindings.filter(looksLikeKeybindingRule) : [];
			readBindingsPath = bindingsPath;
		} catch (err) {
			bindingsReadError = err instanceof Error ? err.message : String(err);
		}
	}
	return { actionsPath, actions, bindings, bindingsPath: readBindingsPath, bindingsReadError };
}

export interface ProjectImportActionRow extends ImportDiffRow {
	write?: UserAction;
}

export interface ProjectImportBindingRow extends ImportDiffRow {
	write?: KeybindingRule;
}

export interface ProjectImportDiff {
	actionRows: ProjectImportActionRow[];
	bindingRows: ProjectImportBindingRow[];
}

/** Classifies a teammate's actions and bindings against the active
 *  project's effective model. `model.projectRoot` must be non-null — the
 *  caller (the Import surface) refuses this source entirely otherwise,
 *  since there is nowhere to write. */
export function buildProjectDiff(source: TeammateProjectSource, model: EffectiveModel): ProjectImportDiff {
	const existingProjectBindings = model.files?.project?.keybindings.document?.bindings ?? [];

	const actionRows: ProjectImportActionRow[] = source.actions.map((action): ProjectImportActionRow => {
		const key = `team-action-${action.id}`;
		if (model.actionById.has(action.id)) {
			return { kind: 'skip', key, title: action.name, detail: 'you already have an action with this id — skipped' };
		}
		const gated = GATED_RUN_KINDS.has(action.run.kind);
		const write: UserAction = {
			...action,
			scope: 'project',
			...(action.icon ? { icon: resolveImportIcon(action.icon) } : {}),
		};
		return {
			kind: 'add',
			key,
			title: action.name,
			detail: gated
				? `${action.run.kind} · runs once this project is trusted (DEC-55)`
				: `${action.run.kind}`,
			write,
		};
	});

	const bindingRows: ProjectImportBindingRow[] = source.bindings.map((rule, i): ProjectImportBindingRow => {
		const key = `team-binding-${i}-${rule.command}-${rule.key}`;
		const negative = rule.command.startsWith('-');
		const already = existingProjectBindings.some(
			(existing) => existing.key === rule.key && existing.command === rule.command && (existing.when ?? '') === (rule.when ?? '')
		);
		if (already) {
			return { kind: 'skip', key, title: `${rule.key} → ${rule.command}`, detail: 'already in this project’s keybindings' };
		}
		if (negative) {
			return {
				kind: 'add',
				key,
				title: `${rule.key} → ${rule.command}`,
				detail: 'held until this project’s keybindings are trusted (DEC-65)',
				write: rule,
			};
		}
		// Same "held is held, whatever holds it" rule as `vscode.ts` (§7.4 rule
		// 1) — the exact-duplicate case is already handled above by `already`.
		const holder = keyHolder(rule.key);
		if (holder) {
			const holderCommand = 'command' in holder ? holder.command : null;
			const heldByLabel = holderCommand ?? 'a predefined system shortcut';
			return {
				kind: 'clash',
				key,
				title: `${rule.key} → ${rule.command}`,
				detail: `already held by \`${heldByLabel}\` — kept yours, imported unbound`,
			};
		}
		return {
			kind: 'add',
			key,
			title: `${rule.key} → ${rule.command}`,
			detail: 'held until this project’s keybindings are trusted (DEC-65)',
			write: rule,
		};
	});

	return { actionRows, bindingRows };
}

/** Writes every `add` row to the active project (DEC-55 §8.3 / DEC-65 B-26
 *  apply automatically from here — nothing extra to do). */
export async function applyProjectImport(diff: ProjectImportDiff): Promise<void> {
	for (const row of diff.actionRows) {
		if (row.kind === 'add' && row.write) {
			await saveUserAction('project', row.write);
		}
	}
	for (const row of diff.bindingRows) {
		if (row.kind === 'add' && row.write) {
			await addKeybinding('project', row.write);
		}
	}
}
