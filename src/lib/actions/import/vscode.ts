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
import { contextKeysOf, normalizeAst, serializeWhen, tryParseWhen, type WhenNode } from '@/lib/keymap/when';
import {
	type ImportDiffRow,
	MAX_IMPORT_FILE_BYTES,
	VSCODE_COMMAND_MAP,
	VSCODE_WHEN_KEY_MAP,
} from './vscode-map';

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

/** Trailing commas before a closing `}` / `]` — the other half of "JSONC".
 *  A tiny string-aware scan (fix round 1, item "low" — the `,` before `]`
 *  inside `"when": "resource =~ '*.ts,*.rs]'"` must never be read as a
 *  trailing comma just because a literal `]` follows it inside the string),
 *  the same string-tracking approach `stripJsonComments` above already uses. */
function stripTrailingCommas(input: string): string {
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
		if (c === ',') {
			// Look ahead past whitespace for a closing `}` / `]` — outside a
			// string, so this never fires on a comma that sits inside one.
			let j = i + 1;
			while (j < n && /\s/.test(input[j])) j++;
			if (j < n && (input[j] === '}' || input[j] === ']')) {
				i++;
				continue;
			}
		}
		out += c;
		i++;
	}
	return out;
}

export interface VSCodeRawRule {
	key?: unknown;
	command?: unknown;
	when?: unknown;
	args?: unknown;
}

/** Parses a VS Code `keybindings.json` body. Throws a message meant to be
 *  shown verbatim (never `ActionsValidationError` — this is a foreign file,
 *  not one of ours). Refuses anything over `MAX_IMPORT_FILE_BYTES` before
 *  doing any comment-stripping or parsing work (fix round 1, item 8). */
export function parseVSCodeKeybindingsText(text: string): VSCodeRawRule[] {
	if (text.length > MAX_IMPORT_FILE_BYTES) {
		return refuseOversized(text.length);
	}
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

function refuseOversized(bytes: number): never {
	const mib = (bytes / (1024 * 1024)).toFixed(1);
	throw new Error(`this file is ${mib} MiB — imports are capped at ${MAX_IMPORT_FILE_BYTES / (1024 * 1024)} MiB`);
}

// ─── Key grammar translation ─────────────────────────────────────────────────

function frozenTable<V>(entries: Readonly<Record<string, V>>): Readonly<Record<string, V>> {
	return Object.assign(Object.create(null) as Record<string, V>, entries);
}

const VSCODE_NAMED_KEYS: Readonly<Record<string, string>> = frozenTable({
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
});

type ModToken = 'ctrl' | 'cmd' | 'winmeta' | 'alt' | 'shift';

const VSCODE_MODIFIER_ALIASES: Readonly<Record<string, ModToken>> = frozenTable({
	ctrl: 'ctrl',
	control: 'ctrl',
	cmd: 'cmd',
	command: 'cmd',
	win: 'winmeta',
	windows: 'winmeta',
	meta: 'winmeta',
	super: 'winmeta',
	alt: 'alt',
	option: 'alt',
	shift: 'shift',
});

/** The platform a VS Code `keybindings.json` was exported from (fix round 1,
 *  item 6). VS Code writes one consistent modifier vocabulary for the whole
 *  file — `cmd` on macOS, `ctrl`/`win` elsewhere — so one `cmd` token
 *  anywhere in the file settles it. */
export type VSCodeSourcePlatform = 'mac' | 'other';

/** Scans every rule's `key` for a `cmd` / `command` token. Absent = `other`
 *  (Windows/Linux): the far more common source for an *imported* file, and
 *  the safe default (§3.1 `ctrl` never silently becomes `mod` on a real mac
 *  file only because this guessed wrong — it only matters when the file
 *  mixes vocabularies, which a real VS Code export never does). */
export function detectVSCodeSourcePlatform(rules: readonly VSCodeRawRule[]): VSCodeSourcePlatform {
	for (const raw of rules) {
		if (typeof raw.key !== 'string') continue;
		for (const stroke of splitKeySequence(raw.key)) {
			for (const tok of stroke.split('+').map((p) => p.trim().toLowerCase())) {
				if (tok === 'cmd' || tok === 'command') return 'mac';
			}
		}
	}
	return 'other';
}

/** One VS Code stroke (`ctrl+k`, `cmd+shift+p`) to our storage grammar
 *  (§3.1), or `null` when it uses a modifier or key token this import
 *  doesn't recognize (numpad keys, `OEM_*` names, … — left "not imported"
 *  rather than guessed at).
 *
 *  Modifier folding is platform-aware (fix round 1, item 6): `mod` is only
 *  ever the *source* file's own platform-primary modifier — `cmd` on a mac
 *  file, `ctrl` on a Windows/Linux file — and the other physical modifier on
 *  that platform (mac `ctrl`; Windows/Linux `win`/`meta`) always comes out
 *  literal, exactly like a live keypress (`key-recorder.tsx`'s `toCanonical`,
 *  §3.1: `mod` can never combine with `ctrl` or `meta`). When both the
 *  primary and the platform's literal modifier are held together, `mod`
 *  can't represent either any more, so both come out literal — the same
 *  rule that already made `ctrl+cmd+p` (a mac file) translate to
 *  `ctrl+meta+p` rather than something `mod` could combine with. `meta` is
 *  never silently folded into `mod` on Windows/Linux, and never silently
 *  dropped on either platform. */
function translateVSCodeStroke(stroke: string, platform: VSCodeSourcePlatform): string | null {
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
	let cmd = false;
	let winMeta = false;
	let alt = false;
	let shift = false;
	for (const tok of modTokens) {
		const resolved = Object.hasOwn(VSCODE_MODIFIER_ALIASES, tok) ? VSCODE_MODIFIER_ALIASES[tok] : undefined;
		if (!resolved) return null;
		if (resolved === 'ctrl') ctrl = true;
		else if (resolved === 'cmd') cmd = true;
		else if (resolved === 'winmeta') winMeta = true;
		else if (resolved === 'alt') alt = true;
		else shift = true;
	}

	let key: string | null = null;
	if (Object.hasOwn(VSCODE_NAMED_KEYS, rawKey)) {
		key = VSCODE_NAMED_KEYS[rawKey];
	} else if (/^f([1-9]|1\d|2[0-4])$/.test(rawKey)) {
		key = rawKey;
	} else if (rawKey.length === 1 && isValidKeyToken(rawKey)) {
		key = rawKey;
	}
	if (!key) return null;

	// literalMods collects every modifier that must stay literal — never
	// folded onto `mod` — on this platform.
	const literalMods: string[] = [];
	if (platform === 'mac') {
		if (ctrl) literalMods.push('ctrl');
		if (winMeta) literalMods.push('meta');
	} else {
		if (winMeta) literalMods.push('meta');
		// An atypical literal `cmd` token in a Windows/Linux file: kept
		// literal too, same rule as `win`/`meta` — never silently `mod`.
		if (cmd) literalMods.push('meta');
	}

	const out: string[] = [];
	const primaryHeld = platform === 'mac' ? cmd : ctrl;
	if (primaryHeld) {
		if (literalMods.length > 0) {
			// The primary modifier is held alongside another literal one —
			// `mod` can't combine with either, so the primary comes out in
			// its own literal form too: `cmd` → `meta` on mac, `ctrl` stays
			// `ctrl` on Windows/Linux (it already is the literal form).
			literalMods.push(platform === 'mac' ? 'meta' : 'ctrl');
		} else {
			out.push('mod');
		}
	}
	out.push(...literalMods);
	if (alt) out.push('alt');
	if (shift) out.push('shift');
	out.push(key);
	return canonicalizeKeySequence(out.join('+'));
}

/** A full VS Code `key` field (one stroke, or a two-stroke chord separated
 *  by one space — the same chord separator as ours) to our storage grammar.
 *  `null` when any stroke is untranslatable, or the result isn't a valid
 *  sequence (more than two strokes, etc.). */
export function translateVSCodeKey(vsCodeKey: string, platform: VSCodeSourcePlatform): string | null {
	const strokes = splitKeySequence(vsCodeKey);
	if (strokes.length === 0 || strokes.length > 2) return null;
	const translated = strokes.map((s) => translateVSCodeStroke(s, platform));
	if (translated.some((s) => s === null)) return null;
	const joined = (translated as string[]).join(' ');
	return validateKeySequence(joined) === null ? joined : null;
}

// ─── `when` translation (the "G-ACTIONS `when` subset") ─────────────────────

export type WhenTranslation = { ok: true; when: string | undefined } | { ok: false; reason: string };

/** Renames every context key named in a `when` AST through `map`, leaving
 *  every literal comparison value (`eq`/`ne`'s `value`, `glob`'s `pattern`)
 *  untouched. Fix round 1, "low" item: the substitution used to be a
 *  `String.replace` over the raw text, which could rewrite a context-key
 *  name that happened to also appear *inside* a quoted string literal
 *  (`resource =~ 'sideBarFocus.ts'`) — walking the already-parsed AST and
 *  touching only `key` fields makes that impossible by construction, no
 *  separate tokenizer needed since `tryParseWhen` already is one. */
function substituteContextKeys(node: WhenNode, map: Readonly<Record<string, string>>): WhenNode {
	switch (node.type) {
		case 'true':
		case 'false':
			return node;
		case 'key':
			return { type: 'key', key: map[node.key] ?? node.key };
		case 'eq':
			return { type: 'eq', key: map[node.key] ?? node.key, value: node.value };
		case 'ne':
			return { type: 'ne', key: map[node.key] ?? node.key, value: node.value };
		case 'glob':
			return { type: 'glob', key: map[node.key] ?? node.key, pattern: node.pattern };
		case 'not':
			return { type: 'not', operand: substituteContextKeys(node.operand, map) };
		case 'and':
			return { type: 'and', operands: node.operands.map((o) => substituteContextKeys(o, map)) };
		case 'or':
			return { type: 'or', operands: node.operands.map((o) => substituteContextKeys(o, map)) };
	}
}

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
	const unmapped = keys.filter((k) => !Object.hasOwn(VSCODE_WHEN_KEY_MAP, k));
	if (unmapped.length > 0) {
		return {
			ok: false,
			reason: `the \`when\` clause uses ${unmapped.length === 1 ? 'a context key' : 'context keys'} outside the supported subset (${unmapped.join(', ')})`,
		};
	}

	try {
		const substituted = substituteContextKeys(parsed.ast, VSCODE_WHEN_KEY_MAP);
		const normalized = serializeWhen(normalizeAst(substituted));
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
	const platform = detectVSCodeSourcePlatform(rules);
	// Fix round 1, item 4: two rows in the *same* import both asking for a
	// key that was free in the effective model at the start of this diff —
	// the second one can't have it either, since Add can only write it once.
	const claimedInThisImport = new Map<string, string>();

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
		if (!Object.hasOwn(VSCODE_COMMAND_MAP, rawCommand)) {
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
		const translatedKey = translateVSCodeKey(rawKey, platform);
		if (!translatedKey) {
			return { kind: 'skip', key, title, detail: `\`${rawKey}\` doesn't translate to a supported key combination` };
		}
		const whenResult = translateVSCodeWhen(typeof raw.when === 'string' ? raw.when : undefined);
		if (!whenResult.ok) {
			return { kind: 'skip', key, title, detail: `${whenResult.reason} — not imported` };
		}

		const claimedBy = claimedInThisImport.get(translatedKey);
		if (claimedBy) {
			return {
				kind: 'clash',
				key,
				title,
				detail: `asks for a key already taken by ${claimedBy} earlier in this import — imports unbound`,
			};
		}

		// §7.4's "held" is agnostic to *what* holds the key (rule 1: "whatever
		// its when") — G-ACTIONS §10.5's own worked example classifies VS
		// Code's "Toggle sidebar" as "imports unbound (`mod+b` held by
		// `explorer.toggle`)" even though `explorer.toggle` is exactly the id
		// this row maps onto. Held-by-the-same-id it is about to bind is the
		// harmless case, though (orchestrator decision, fix round 1 item 7,
		// matching `package.ts`'s identical rule): it is reported `skip`
		// ("already bound"), not `clash` — both mean nothing is written, so
		// this changes only how the row reads, not what happens on Add.
		const holder = keyHolder(translatedKey);
		if (holder) {
			const holderCommand = 'command' in holder ? holder.command : null;
			if (holderCommand === mappedId) {
				return { kind: 'skip', key, title, detail: `already bound to \`${mappedId}\`` };
			}
			const heldByLabel = holderCommand ?? 'a predefined system shortcut';
			return {
				kind: 'clash',
				key,
				title,
				detail: `asks for a key already held by \`${heldByLabel}\` — kept yours, \`${mappedId}\` imports unbound`,
			};
		}

		claimedInThisImport.set(translatedKey, `\`${title}\``);
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
