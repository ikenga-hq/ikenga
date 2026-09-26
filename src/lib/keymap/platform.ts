// Key grammar (G-ACTIONS §3.1, DEC-61), key-sequence parsing, matching and
// display formatting.
//
// Grammar: a key sequence is one stroke, or a two-stroke chord separated by
// one space (`mod+k mod+r`). A stroke is modifiers joined by `+`, then the
// key: `mod+1`, `mod+shift+n`, `ctrl+t`. Lowercase; each modifier at most
// once; `mod` is never combined with `ctrl` or `meta`.
// - `mod` is the platform-primary modifier (⌘ on macOS, Ctrl elsewhere).
// - `ctrl` is the *literal* Control key on every platform (the spec's ⌃T,
//   ⌃1..⌃6 are deliberately not platform-swapped).
// - `meta` is the literal ⌘ / Win / Super key.
// The key `+` is spelled `plus`, space is `space`. Shifted punctuation names
// the **unshifted** key: ⌘⇧[ is `mod+shift+[`.
//
// Canonical (stored) form: modifiers in the order mod, ctrl, meta, alt,
// shift, then the key (`canonicalizeKeySequence`). The *resolved* form —
// what `conflicts()` groups on and what an event produces — has `mod`
// replaced by `meta` (macOS) or `ctrl` (elsewhere) and orders modifiers
// meta, ctrl, alt, shift (`resolveCombo`).
//
// ── `key` vs `code` (WP-49's documented choice for non-US layouts) ────────
// Browsers report both `KeyboardEvent.key` (the character the layout
// produced) and `KeyboardEvent.code` (the physical position, named after the
// US layout). Neither alone is right:
// - `key` alone breaks shifted punctuation (⌘⇧[ arrives as `{` on US) and
//   anything a modifier transforms (macOS ⌥1 → `¡`, ⌥A → `å`), and gives
//   non-Latin layouts no Latin letter at all (Cyrillic ⌘C → `с`).
// - `code` alone breaks layout-labelled letters: on AZERTY the key labelled
//   Z sits at `KeyW`, so ⌘Z would undo on the key labelled W.
// So `strokesFromEvent` uses a **hybrid**: a named key by its name; an ASCII
// letter or digit by the produced `key` (layout-labelled — ⌘Z is the key
// that says Z); an unshifted, unmodified-by-Alt ASCII punctuation character
// in the grammar by `key`; **everything else by `code`**, mapped to the US
// key name (Shift-produced punctuation, Alt-produced characters, AZERTY's
// shifted digit row, non-Latin letters). The storage grammar does not change
// with this choice (§3.1).
// Two character keys are the exception, matched on the produced character
// with Shift ignored: `?` (the shipped `shortcuts.open-quick`) and `plus`
// (`+` is Shift+= on US but unshifted on other layouts and on the numpad).
// A `?` / `+` event therefore yields two candidate strokes — the character
// form and the positional form — and a rule matching either fires.
//
// ── Keyboard robustness (§3.3) ────────────────────────────────────────────
// An event with `isComposing` or `keyCode === 229` (IME composition), a
// `Dead` key, or a bare modifier press yields no stroke and never matches.

import { isMac as isMacDefault } from '@/lib/platform';

export function isMacPlatform(): boolean {
	return isMacDefault;
}

// ─── Grammar ──────────────────────────────────────────────────────────────

export const MODIFIERS = ['mod', 'ctrl', 'meta', 'alt', 'shift'] as const;
export type Modifier = (typeof MODIFIERS)[number];

const PUNCT = new Set(['`', '-', '=', '[', ']', '\\', ';', "'", ',', '.', '/', '?']);
const NAMED = new Set([
	'enter',
	'escape',
	'tab',
	'space',
	'backspace',
	'delete',
	'insert',
	'home',
	'end',
	'pageup',
	'pagedown',
	'arrowup',
	'arrowdown',
	'arrowleft',
	'arrowright',
	'plus',
	...Array.from({ length: 24 }, (_, i) => `f${i + 1}`),
]);

/** Is `key` a valid key token (§3.1 `key` production)? */
export function isValidKeyToken(key: string): boolean {
	if (key.length === 1) {
		return (key >= 'a' && key <= 'z') || (key >= '0' && key <= '9') || PUNCT.has(key);
	}
	return NAMED.has(key);
}

export interface ParsedCombo {
	mods: Set<string>;
	key: string;
}

/**
 * Split one stroke into modifiers + key. Lenient (it never throws): used on
 * every read path. `validateKeySequence` is the strict check. A trailing
 * literal `+` (`mod++`) is read as the `plus` key.
 */
export function parseCombo(combo: string): ParsedCombo {
	const trimmed = combo.trim().toLowerCase();
	const endsWithPlus = trimmed.length > 1 && trimmed.endsWith('++');
	const parts = (endsWithPlus ? trimmed.slice(0, -2) : trimmed)
		.split('+')
		.map((p) => p.trim())
		.filter((p) => p.length > 0);
	if (endsWithPlus) parts.push('plus');
	const key = parts[parts.length - 1] ?? '';
	const mods = new Set(parts.slice(0, -1));
	return { mods, key };
}

/** The strokes of a key sequence: one, or two for a chord. */
export function splitKeySequence(seq: string): string[] {
	return seq
		.trim()
		.split(/\s+/)
		.filter((s) => s.length > 0);
}

/** Is `seq` a two-stroke chord? */
export function isChordSequence(seq: string): boolean {
	return splitKeySequence(seq).length === 2;
}

/**
 * Strict §3.1 validation — the FE mirror of the validator's `E_KEY_GRAMMAR`.
 * Returns `null` when valid, else a reason.
 */
export function validateKeySequence(seq: string): string | null {
	if (seq !== seq.trim() || /\s{2,}/.test(seq)) return 'strokes are separated by exactly one space';
	const strokes = seq.split(' ');
	if (strokes.length === 0 || strokes.length > 2) return 'one stroke, or a two-stroke chord';
	for (const stroke of strokes) {
		if (stroke !== stroke.toLowerCase()) return `"${stroke}" must be lowercase`;
		if (stroke.length === 0) return 'empty stroke';
		const parts = stroke.split('+');
		const key = parts[parts.length - 1];
		const mods = parts.slice(0, -1);
		if (!key) return `"${stroke}" has no key (spell "+" as "plus")`;
		if (!isValidKeyToken(key)) return `unknown key "${key}"`;
		const seen = new Set<string>();
		for (const m of mods) {
			if (!(MODIFIERS as readonly string[]).includes(m)) return `unknown modifier "${m}"`;
			if (seen.has(m)) return `modifier "${m}" repeated`;
			seen.add(m);
		}
		if (seen.has('mod') && (seen.has('ctrl') || seen.has('meta'))) {
			return '"mod" cannot be combined with "ctrl" or "meta"';
		}
	}
	return null;
}

/** Canonical stored form: modifiers ordered mod, ctrl, meta, alt, shift. */
export function canonicalizeKeySequence(seq: string): string {
	return splitKeySequence(seq)
		.map((stroke) => {
			const { mods, key } = parseCombo(stroke);
			return [...MODIFIERS.filter((m) => mods.has(m)), key].join('+');
		})
		.join(' ');
}

// ─── Resolution (`mod` → platform modifier) ───────────────────────────────

function resolvedModifiers(mods: Set<string>, mac: boolean) {
	const usesMod = mods.has('mod');
	return {
		meta: usesMod ? mac || mods.has('meta') : mods.has('meta'),
		ctrl: usesMod ? !mac || mods.has('ctrl') : mods.has('ctrl'),
		shift: mods.has('shift'),
		alt: mods.has('alt'),
	};
}

function joinResolved(m: { meta: boolean; ctrl: boolean; alt: boolean; shift: boolean }, key: string) {
	return [m.meta && 'meta', m.ctrl && 'ctrl', m.alt && 'alt', m.shift && 'shift', key]
		.filter((v): v is string => Boolean(v))
		.join('+');
}

/** Canonical, platform-resolved signature for one stroke — used by
 *  `conflicts()` to group bindings that land on the same physical keys on a
 *  given platform, even when spelled differently (`mod+t` vs `ctrl+t`). */
export function resolveCombo(combo: string, mac: boolean): string {
	const { mods, key } = parseCombo(combo);
	return joinResolved(resolvedModifiers(mods, mac), key);
}

/** `resolveCombo` over every stroke of a sequence, space-joined. */
export function resolveKeySequence(seq: string, mac: boolean): string {
	return splitKeySequence(seq)
		.map((s) => resolveCombo(s, mac))
		.join(' ');
}

// ─── Events → strokes ─────────────────────────────────────────────────────

export type KeyEventLike = Pick<KeyboardEvent, 'key' | 'metaKey' | 'ctrlKey' | 'shiftKey' | 'altKey'> &
	Partial<Pick<KeyboardEvent, 'code' | 'isComposing' | 'keyCode'>>;

const MODIFIER_KEYS = new Set(['shift', 'control', 'meta', 'alt', 'altgraph', 'os', 'hyper', 'super', 'capslock', 'fn']);

const EVENT_KEY_NAMES: Record<string, string> = {
	enter: 'enter',
	escape: 'escape',
	esc: 'escape',
	tab: 'tab',
	' ': 'space',
	spacebar: 'space',
	backspace: 'backspace',
	delete: 'delete',
	del: 'delete',
	insert: 'insert',
	home: 'home',
	end: 'end',
	pageup: 'pageup',
	pagedown: 'pagedown',
	arrowup: 'arrowup',
	arrowdown: 'arrowdown',
	arrowleft: 'arrowleft',
	arrowright: 'arrowright',
	up: 'arrowup',
	down: 'arrowdown',
	left: 'arrowleft',
	right: 'arrowright',
};

/** `KeyboardEvent.code` → US key name (the positional fallback). */
function keyFromCode(code: string | undefined): string | null {
	if (!code) return null;
	if (code.startsWith('Key') && code.length === 4) return code.slice(3).toLowerCase();
	if (code.startsWith('Digit') && code.length === 6) return code.slice(5);
	if (code.startsWith('Numpad') && code.length === 7 && code[6] >= '0' && code[6] <= '9') return code.slice(6);
	const table: Record<string, string> = {
		Backquote: '`',
		Minus: '-',
		Equal: '=',
		BracketLeft: '[',
		BracketRight: ']',
		Backslash: '\\',
		IntlBackslash: '\\',
		Semicolon: ';',
		Quote: "'",
		Comma: ',',
		Period: '.',
		Slash: '/',
		NumpadAdd: 'plus',
		NumpadSubtract: '-',
		NumpadDecimal: '.',
		NumpadDivide: '/',
		NumpadEnter: 'enter',
		Space: 'space',
	};
	return table[code] ?? null;
}

function isAsciiLetterOrDigit(c: string): boolean {
	return (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9');
}

/**
 * The resolved strokes (`meta+ctrl+alt+shift+key` order, no `mod`) a key
 * event can match, most specific first — usually exactly one; two for the
 * `?` / `plus` character keys (see the header). Returns `[]` for an event
 * that must never match: IME composition, a `Dead` key, a bare modifier, or
 * a key with no mapping.
 */
export function strokesFromEvent(e: KeyEventLike): string[] {
	if (e.isComposing || e.keyCode === 229) return [];
	const raw = e.key ?? '';
	if (raw === 'Dead' || raw === 'Process' || raw === '') return [];
	const lower = raw.toLowerCase();
	if (MODIFIER_KEYS.has(lower)) return [];

	const mods = { meta: e.metaKey, ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey };
	const out: string[] = [];

	// Character keys matched on the produced character, Shift ignored.
	if (raw === '?' || raw === '+') {
		out.push(joinResolved({ ...mods, shift: false }, raw === '?' ? '?' : 'plus'));
	}

	let key: string | null = null;
	if (EVENT_KEY_NAMES[lower]) key = EVENT_KEY_NAMES[lower];
	else if (/^f([1-9]|1[0-9]|2[0-4])$/.test(lower)) key = lower;
	else if (lower.length === 1 && isAsciiLetterOrDigit(lower)) key = lower;
	else if (raw.length === 1 && PUNCT.has(raw) && !e.shiftKey && !e.altKey && raw !== '?') key = raw;
	else key = keyFromCode(e.code);

	if (key) {
		const stroke = joinResolved(mods, key);
		if (!out.includes(stroke)) out.push(stroke);
	}
	return out;
}

/** Does `e` match the single stroke `combo` on this platform? A chord never
 *  matches a single event (the chord machine in `chord.ts` handles those). */
export function eventMatchesCombo(e: KeyEventLike, combo: string, mac: boolean): boolean {
	if (isChordSequence(combo)) return false;
	const target = resolveCombo(combo, mac);
	return strokesFromEvent(e).includes(target);
}

// ─── Display ──────────────────────────────────────────────────────────────

const NAMED_KEYS: Record<string, string> = {
	enter: '⏎',
	escape: 'Esc',
	backspace: '⌫',
	delete: '⌦',
	tab: 'Tab',
	space: 'Space',
	plus: '+',
	arrowup: '↑',
	arrowdown: '↓',
	arrowleft: '←',
	arrowright: '→',
	pageup: 'PgUp',
	pagedown: 'PgDn',
};

function keyGlyph(key: string): string {
	if (key.length === 1) return key.toUpperCase();
	if (/^f\d+$/.test(key)) return key.toUpperCase();
	return NAMED_KEYS[key] ?? key.charAt(0).toUpperCase() + key.slice(1);
}

function formatStroke(combo: string, mac: boolean): string {
	const { mods, key } = parseCombo(combo);
	const { meta, ctrl, alt, shift } = resolvedModifiers(mods, mac);
	const glyph = keyGlyph(key);
	if (mac) {
		// Matches the glyph order already shipped across the codebase
		// (`⌃⇧T`, `⌘⇧N`, `⌘⇧\`): ⌃, ⌘, then ⌥, then ⇧.
		let out = '';
		if (ctrl) out += '⌃';
		if (meta) out += '⌘';
		if (alt) out += '⌥';
		if (shift) out += '⇧';
		return out + glyph;
	}
	const parts: string[] = [];
	if (ctrl) parts.push('Ctrl');
	if (meta) parts.push('Win');
	if (alt) parts.push('Alt');
	if (shift) parts.push('Shift');
	parts.push(glyph);
	return parts.join('+');
}

/** Human-readable label for a key sequence — `labelFor()`'s formatting
 *  engine. macOS concatenates symbols (⌘⇧T); other platforms spell modifiers
 *  out (Ctrl+Shift+T). A chord's two strokes are space-separated (⌘K ⌘R). */
export function formatKeyLabel(combo: string, opts?: { mac?: boolean }): string {
	const mac = opts?.mac ?? isMacPlatform();
	return splitKeySequence(combo)
		.map((s) => formatStroke(s, mac))
		.join(' ');
}

const ACCELERATOR_KEYS: Record<string, string> = {
	enter: 'Enter',
	escape: 'Escape',
	tab: 'Tab',
	space: 'Space',
	backspace: 'Backspace',
	delete: 'Delete',
	insert: 'Insert',
	home: 'Home',
	end: 'End',
	pageup: 'PageUp',
	pagedown: 'PageDown',
	arrowup: 'Up',
	arrowdown: 'Down',
	arrowleft: 'Left',
	arrowright: 'Right',
	plus: 'Plus',
};

/** Tauri `accelerator` syntax (`CmdOrCtrl+Shift+X`) for native-menu items.
 *  A native accelerator cannot express a chord, so a two-stroke sequence
 *  returns `undefined` (the item shows no accelerator; the registry still
 *  fires it) — Tauri rejects an empty-string `accelerator`. */
export function toAccelerator(combo: string): string | undefined {
	if (isChordSequence(combo)) return undefined;
	const { mods, key } = parseCombo(combo);
	const parts: string[] = [];
	if (mods.has('mod')) parts.push('CmdOrCtrl');
	if (mods.has('ctrl') && !mods.has('mod')) parts.push('Ctrl');
	if (mods.has('meta') && !mods.has('mod')) parts.push('Cmd');
	if (mods.has('alt')) parts.push('Alt');
	if (mods.has('shift')) parts.push('Shift');
	parts.push(ACCELERATOR_KEYS[key] ?? key.toUpperCase());
	return parts.join('+');
}
