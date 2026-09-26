// WP-61 — "From VS Code keybindings" import source (G-ACTIONS §5, §10;
// routine brief item 24). Reads a VS Code `keybindings.json` (JSONC-tolerant:
// comments and trailing commas, same as the file VS Code itself writes —
// Ikenga's OWN `keybindings.json` stays strict JSON, G-ACTIONS §1.1, and this
// parser is never used on it), classifies every rule against the id-map core
// (`vscode-map.ts`) and the current effective model, and — on Add — writes
// only new `KeybindingRule`s, at whichever scope the Import surface's shared
// Personal/Project switch is on. Nothing here creates a `UserAction`: every
// mapped id already names a live built-in, so there is no action to author,
// only a key to bind.

import { keyHolder, addKeybinding, type KeybindingRule } from '@/lib/actions/store';
import {
	canonicalizeKeySequence,
	isValidKeyToken,
	splitKeySequence,
	validateKeySequence,
} from '@/lib/keymap/platform';
import { contextKeysOf, normalizeWhen, tryParseWhen } from '@/lib/keymap/when';
import { type ImportDiffRow, VSCODE_COMMAND_MAP, VSCODE_WHEN_KEY_MAP } from './vscode-map';

// ─── JSONC-tolerant read (input only, §1.1) ─────────────────────────────────

/** Strips `//` and `/* … *‍/` comments outside string literals. VS Code's own
 *  `keybindings.json` is JSON-with-comments; ours never is. */
function stripJsonComments(input: string): string {
	let out = '';
	let i = 0;
	const n = input.length;
	let inString = false;
	while (i < n) {
		const c = input[i];
		if (inString) {
			out += c;
			if (c === '\\' && i + 1 < n) {
				out += input[i + 1];
				i += 2;
				continue;
			}
			if (c === '"') inString = false;
			i++;
			continue;
		}
		if (c === '"') {
			inString = true;
			out += c;
			i++;
			continue;
		}
		if (c === '/' && input[i + 1] === '/') {
			i += 2;
			while (i < n && input[i] !== '\n') i++;
			continue;
		}
		if (c === '/' && input[i + 1] === '*') {
			i += 2;
			while (i < n && !(input[i] === '*' && input[i + 1] === '/')) i++;
			i += 2;
			continue;
		}
		out += c;
		i++;
	}
	return out;
}

/** Trailing commas before a closing `}` / `]` — the other half of "JSONC". */
function stripTrailingCommas(input: string): string {
	return input.replace(/,(\s*[}\]])/g, '$1');
}

export interface VSCodeRawRule {
	key?: unknown;
	command?: unknown;
	when?: unknown;
	args?: unknown;
}

/** Parses a VS Code `keybindings.json` body. Throws a message meant to be
 *  shown verbatim (never `ActionsValidationError` — this is a foreign file,
 *  not one of ours). */
export function parseVSCodeKeybindingsText(text: string): VSCodeRawRule[] {
	const stripped = stripTrailingCommas(stripJsonComments(text));
	let parsed: unknown;
	try {
		parsed = JSON.parse(stripped);
	} catch (err) {
		throw new Error(`not valid JSON (after stripping comments): ${err instanceof Error ? err.message : String(err)}`);
	}
	if (!Array.isArray(parsed)) {
		throw new Error('a VS Code keybindings.json is a top-level array of {key, command, when?} entries');
	}
	return parsed as VSCodeRawRule[];
}

// ─── Key grammar translation ─────────────────────────────────────────────────

const VSCODE_NAMED_KEYS: Readonly<Record<string, string>> = {
	up: 'arrowup',
	down: 'arrowdown',
	left: 'arrowleft',
	right: 'arrowright',
	esc: 'escape',
	escape: 'escape',
	enter: 'enter',
	return: 'enter',
	tab: 'tab',
	backspace: 'backspace',
	del: 'delete',
	delete: 'delete',
	space: 'space',
	pageup: 'pageup',
	pagedown: 'pagedown',
	home: 'home',
	end: 'end',
	insert: 'insert',
};

const VSCODE_MODIFIER_ALIASES: Readonly<Record<string, 'ctrl' | 'meta' | 'alt' | 'shift'>> = {
	ctrl: 'ctrl',
	control: 'ctrl',
	cmd: 'meta',
	command: 'meta',
	win: 'meta',
	windows: 'meta',
	meta: 'meta',
	super: 'meta',
	alt: 'alt',
	option: 'alt',
	shift: 'shift',
};

/** One VS Code stroke (`ctrl+k`, `cmd+shift+p`) to our storage grammar
 *  (§3.1), or `null` when it uses a modifier or key token this import
 *  doesn't recognize (numpad keys, `OEM_*` names, … — left "not imported"
 *  rather than guessed at). `ctrl` and `cmd`/`win`/`meta` each fold onto the
 *  platform-primary `mod` on their own; holding both together is the one
 *  case that must stay literal (`mod` can never combine with `ctrl`/`meta`,
 *  §3.1) — the same rule `key-recorder.tsx`'s `toCanonical` applies to a
 *  live keypress. */
function translateVSCodeStroke(stroke: string): string | null {
	const parts = stroke
		.trim()
		.toLowerCase()
		.split('+')
		.map((p) => p.trim())
		.filter((p) => p.length > 0);
	if (parts.length === 0) return null;
	const rawKey = parts[parts.length - 1];
	const modTokens = parts.slice(0, -1);

	let ctrl = false;
	let meta = false;
	let alt = false;
	let shift = false;
	for (const tok of modTokens) {
		const resolved = VSCODE_MODIFIER_ALIASES[tok];
		if (!resolved) return null;
		if (resolved === 'ctrl') ctrl = true;
		else if (resolved === 'meta') meta = true;
		else if (resolved === 'alt') alt = true;
		else shift = true;
	}

	let key: string | null = null;
	if (VSCODE_NAMED_KEYS[rawKey]) {
		key = VSCODE_NAMED_KEYS[rawKey];
	} else if (/^f([1-9]|1\d|2[0-4])$/.test(rawKey)) {
		key = rawKey;
	} else if (rawKey.length === 1 && isValidKeyToken(rawKey)) {
		key = rawKey;
	}
	if (!key) return null;

	const out: string[] = [];
	if (ctrl && meta) {
		out.push('ctrl', 'meta');
	} else if (ctrl || meta) {
		out.push('mod');
	}
	if (alt) out.push('alt');
	if (shift) out.push('shift');
	out.push(key);
	return canonicalizeKeySequence(out.join('+'));
}

/** A full VS Code `key` field (one stroke, or a two-stroke chord separated
 *  by one space — the same chord separator as ours) to our storage grammar.
 *  `null` when any stroke is untranslatable, or the result isn't a valid
 *  sequence (more than two strokes, etc.). */
export function translateVSCodeKey(vsCodeKey: string): string | null {
	const strokes = splitKeySequence(vsCodeKey);
	if (strokes.length === 0 || strokes.length > 2) return null;
	const translated = strokes.map(translateVSCodeStroke);
	if (translated.some((s) => s === null)) return null;
	const joined = (translated as string[]).join(' ');
	return validateKeySequence(joined) === null ? joined : null;
}

// ─── `when` translation (the "G-ACTIONS `when` subset") ─────────────────────

export type WhenTranslation = { ok: true; when: string | undefined } | { ok: false; reason: string };

/**
 * Translates a VS Code `when` clause into DEC-62 form, or reports it as
 * outside the supported subset. VS Code's `when` grammar is close enough to
 * ours (`!`, `&&`, `||`, `==`, `!=`, parens, quoted strings) that the shared
 * parser (`@/lib/keymap/when`, WP-49) reads it directly; this function only
 * adds the VS Code → Ikenga context-key substitution (`VSCODE_WHEN_KEY_MAP`)
 * and refuses anything naming a key outside that small table — it never
 * imports a `when` it can't fully translate, which would otherwise silently
 * widen (or narrow) when a binding fires.
 */
export function translateVSCodeWhen(clause: string | undefined | null): WhenTranslation {
	const trimmed = (clause ?? '').trim();
	if (trimmed === '') return { ok: true, when: undefined };

	const parsed = tryParseWhen(trimmed);
	if (!parsed.ok) return { ok: false, reason: `the \`when\` clause doesn't parse: ${parsed.error.message}` };

	const keys = contextKeysOf(parsed.ast);
	const unmapped = keys.filter((k) => !VSCODE_WHEN_KEY_MAP[k]);
	if (unmapped.length > 0) {
		return {
			ok: false,
			reason: `the \`when\` clause uses ${unmapped.length === 1 ? 'a context key' : 'context keys'} outside the supported subset (${unmapped.join(', ')})`,
		};
	}

	let substituted = trimmed;
	for (const key of keys) {
		substituted = substituted.replace(new RegExp(`\\b${key}\\b`, 'g'), VSCODE_WHEN_KEY_MAP[key]);
	}
	try {
		const normalized = normalizeWhen(substituted);
		return { ok: true, when: normalized === '' ? undefined : normalized };
	} catch (err) {
		return { ok: false, reason: `the translated \`when\` clause didn't normalize: ${err instanceof Error ? err.message : String(err)}` };
	}
}

// ─── Diff ────────────────────────────────────────────────────────────────────

export interface VSCodeImportRow extends ImportDiffRow {
	/** Set only for `kind: 'add'` — the rule `applyVSCodeImport` writes. */
	write?: KeybindingRule;
}

/**
 * Classifies every parsed rule against the id-map core and the current
 * effective model (G-ACTIONS §5 "import is a writer, not a layer"; §14 item
 * 24: "a clash never overrides an existing binding; the VS Code mapping
 * table is checked in, unmapped commands are skipped with a reason").
 */
export function buildVSCodeDiff(rules: readonly VSCodeRawRule[]): VSCodeImportRow[] {
	return rules.map((raw, i): VSCodeImportRow => {
		const rawCommand = typeof raw.command === 'string' ? raw.command : null;
		const rawKey = typeof raw.key === 'string' ? raw.key : null;
		const key = `vscode-${i}-${rawCommand ?? '?'}-${rawKey ?? '?'}`;
		const title = rawCommand ?? '(missing command)';

		if (!rawCommand || !rawKey) {
			return { kind: 'skip', key, title, detail: 'missing `key` or `command` — not imported' };
		}
		if (rawCommand.startsWith('-')) {
			return { kind: 'skip', key, title, detail: 'removes a VS Code default binding — nothing to import' };
		}
		if (!(rawCommand in VSCODE_COMMAND_MAP)) {
			return {
				kind: 'skip',
				key,
				title,
				detail: `\`${rawCommand}\` is not in the frozen VS Code command map — not imported`,
			};
		}
		const mappedId = VSCODE_COMMAND_MAP[rawCommand];
		if (mappedId === null) {
			return { kind: 'skip', key, title, detail: `no Ikenga equivalent for \`${rawCommand}\`` };
		}
		const translatedKey = translateVSCodeKey(rawKey);
		if (!translatedKey) {
			return { kind: 'skip', key, title, detail: `\`${rawKey}\` doesn't translate to a supported key combination` };
		}
		const whenResult = translateVSCodeWhen(typeof raw.when === 'string' ? raw.when : undefined);
		if (!whenResult.ok) {
			return { kind: 'skip', key, title, detail: `${whenResult.reason} — not imported` };
		}

		// §7.4's "held" is agnostic to *what* holds the key (rule 1: "whatever
		// its command") — G-ACTIONS §10.5's own worked example classifies VS
		// Code's "Toggle sidebar" as "imports unbound (`mod+b` held by
		// `explorer.toggle`)" even though `explorer.toggle` is exactly the id
		// this row maps onto, so this never special-cases "held by the same
		// command I'm about to bind" as a no-op skip — any holder is a clash.
		const holder = keyHolder(translatedKey);
		if (holder) {
			const holderCommand = 'command' in holder ? holder.command : null;
			const heldByLabel = holderCommand ?? 'a predefined system shortcut';
			return {
				kind: 'clash',
				key,
				title,
				detail: `asks for a key already held by \`${heldByLabel}\` — kept yours, \`${mappedId}\` imports unbound`,
			};
		}

		const rule: KeybindingRule = { key: translatedKey, command: mappedId, ...(whenResult.when ? { when: whenResult.when } : {}) };
		return {
			kind: 'add',
			key,
			title,
			detail: `→ \`${mappedId}\``,
			write: rule,
		};
	});
}

/** Writes every `add` row's rule to `scope`. Nothing is written for `skip` /
 *  `clash` rows (§5: import never overrides). A project-scope write's
 *  bindings are held until the project's keybindings are trusted (DEC-65) —
 *  automatic, since the write changes the file's `bindings` hash the same
 *  way any other edit does; no separate "write held" step exists. */
export async function applyVSCodeImport(rows: readonly VSCodeImportRow[], scope: 'personal' | 'project'): Promise<void> {
	for (const row of rows) {
		if (row.kind === 'add' && row.write) {
			await addKeybinding(scope, row.write);
		}
	}
}
