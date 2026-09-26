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
import { validateKeySequence } from '@/lib/keymap/platform';
import type { ImportDiffRow } from './vscode-map';
import { MAX_IMPORT_FILE_BYTES, resolveImportIcon } from './vscode-map';

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

/** Fix round 1, item 8: refuse before ever decoding/parsing — same cap and
 *  wording as the VS Code source (`vscode.ts`'s `parseVSCodeKeybindingsText`). */
function refuseOversized(bytes: number): never {
	const mib = (bytes / (1024 * 1024)).toFixed(1);
	throw new Error(`this file is ${mib} MiB — imports are capped at ${MAX_IMPORT_FILE_BYTES / (1024 * 1024)} MiB`);
}

/** Reads a teammate's `actions.json` (required) and its sibling
 *  `keybindings.json` (best-effort). Throws only when `actionsPath` itself
 *  can't be read or parsed — a failed sibling read is reported in the
 *  result, not thrown, since a project can have actions with no
 *  keybindings file at all. */
export async function readTeammateProjectFile(actionsPath: string): Promise<TeammateProjectSource> {
	const actionsBytes = (await fsRead(actionsPath)).bytes;
	if (actionsBytes.length > MAX_IMPORT_FILE_BYTES) refuseOversized(actionsBytes.length);
	const raw = decode(actionsBytes);
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
			const bindingsBytes = (await fsRead(bindingsPath)).bytes;
			if (bindingsBytes.length > MAX_IMPORT_FILE_BYTES) refuseOversized(bindingsBytes.length);
			const bindingsRaw = decode(bindingsBytes);
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

	// Fix round 1, item 4: two rows in the same import asking for the same
	// free key — the second one can't have it either.
	const claimedInThisImport = new Map<string, string>();

	const bindingRows: ProjectImportBindingRow[] = source.bindings.map((rawRule, i): ProjectImportBindingRow => {
		const key = `team-binding-${i}-${rawRule.command}-${rawRule.key}`;

		// §10.1: the `action:<id>` command form is D-06's display-only preview
		// shape and is never valid in a file — strip it if a hand-edited or
		// copy-pasted teammate file carries it, so the bare id underneath still
		// imports (fix round 1, item 10).
		const command = rawRule.command.startsWith('action:') ? rawRule.command.slice('action:'.length) : rawRule.command;
		const rule: KeybindingRule = command === rawRule.command ? rawRule : { ...rawRule, command };
		const title = `${rule.key} → ${rule.command}`;

		// Fix round 1, item 10: only the personal file may carry an OS-wide
		// rule (DEC-60, G-ACTIONS §6) — a project file's `scope: "os"` is
		// `E_OS_LAYER` at write time, so it is never imported in the first
		// place, not written and refused.
		if (rule.scope === 'os') {
			return { kind: 'skip', key, title, detail: 'OS-wide rules may only live in your personal file (DEC-60) — not imported' };
		}

		// Fix round 1, item 10: validate the key grammar before ever calling
		// `keyHolder` on it — an unparseable key can't be "held" or "free",
		// it's just invalid.
		const keyError = validateKeySequence(rule.key);
		if (keyError) {
			return { kind: 'skip', key, title, detail: `invalid key — ${keyError}` };
		}

		const negative = rule.command.startsWith('-');
		const already = existingProjectBindings.some(
			(existing) => existing.key === rule.key && existing.command === rule.command && (existing.when ?? '') === (rule.when ?? '')
		);
		if (already) {
			return { kind: 'skip', key, title, detail: 'already in this project’s keybindings' };
		}
		// Fix round 1, item 3 (orchestrator decision): a negative rule removes
		// a binding — it is never an add, whatever the target. DEC-65 names
		// `-pane.close` from an untrusted project file as exactly the threat
		// this guards against (an import is a writer, never a layer, §5: it
		// never overrides an existing binding — and unbinding one is exactly
		// that, just via a negative rule instead of a clashing positive one).
		if (negative) {
			return { kind: 'skip', key, title, detail: 'removes a binding — not imported' };
		}

		const claimedBy = claimedInThisImport.get(rule.key);
		if (claimedBy) {
			return { kind: 'clash', key, title, detail: `asks for a key already taken by ${claimedBy} earlier in this import — imported unbound` };
		}

		// Same "held is held, whatever holds it" rule as `vscode.ts` (§7.4 rule
		// 1) — the exact-duplicate case is already handled above by `already`.
		const holder = keyHolder(rule.key);
		if (holder) {
			const holderCommand = 'command' in holder ? holder.command : null;
			// Fix round 1, item 7 (orchestrator decision, matching
			// `package.ts:58`): held by the *same* command the row is asking
			// to bind is the harmless case — skip it as "already bound"
			// rather than reporting it as a clash.
			if (holderCommand === rule.command) {
				return { kind: 'skip', key, title, detail: 'already bound' };
			}
			const heldByLabel = holderCommand ?? 'a predefined system shortcut';
			return {
				kind: 'clash',
				key,
				title,
				detail: `already held by \`${heldByLabel}\` — kept yours, imported unbound`,
			};
		}
		claimedInThisImport.set(rule.key, `\`${title}\``);
		return {
			kind: 'add',
			key,
			title,
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
