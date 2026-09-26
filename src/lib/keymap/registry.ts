// The one keymap registry (§6A.5): `{ command, key, when, source }`. Every
// consumer that shows or fires a key binding — the rail, the command
// palette, pane chrome, the native menu, and (later) the Shortcuts view and
// the Keys editor — reads through here instead of hard-coding a label.
//
// Keymap v2 (WP-49, G-ACTIONS §2–§5): `when` is a DEC-62 expression
// (`when.ts`) evaluated against the context-key service (`context-keys.ts`);
// `conflicts()` follows DEC-59 (normalized-`when` comparison, clash vs
// precedence, OS scope as its own space); chords are handled by `chord.ts`.
// `getKeymap()` returns the **effective keymap** (WP-52, G-ACTIONS §2.2):
// defaults < granted package requests < personal < trusted project rules,
// negative rules applied per platform. The effective model
// (`src/lib/actions/store.ts`) computes it and publishes it here with
// `setEffectiveKeymap()`; until the model has loaded (and in tests that never
// start it) it is the defaults. Callers never read `DEFAULT_KEYMAP` directly.

import { useEffect } from 'react';
import { type ContextKeys, getContextKeys, getEvalOptions } from './context-keys';
import { DEFAULT_KEYMAP, type KeymapEntry, type KeymapScope, type KeymapSource } from './defaults';
import {
	canonicalizeKeySequence,
	eventMatchesCombo,
	formatKeyLabel,
	isMacPlatform,
	resolveKeySequence,
} from './platform';
import { evaluateWhen, normalizeWhen, whenSpecificity } from './when';

export type { KeymapEntry, KeymapRuleOrigin, KeymapScope, KeymapSource } from './defaults';
export type { WhenClause } from './when';
export { isTypingTarget } from './when';

let effectiveKeymap: KeymapEntry[] | null = null;
const keymapListeners = new Set<() => void>();

/**
 * The active keymap: the effective merge of every layer (G-ACTIONS §2.2),
 * in merge order (default, package, personal, project — the order §2.3's
 * "latest in merge order" tie-break reads). Entries carry `platformOnly`
 * narrowed where a negative rule removed a binding on one platform only, so
 * `entriesForPlatform()` still yields each platform's effective list. Held
 * project rules (DEC-65) are never in it. Defaults until the effective model
 * publishes a merge.
 */
export function getKeymap(): KeymapEntry[] {
	return effectiveKeymap ?? DEFAULT_KEYMAP;
}

/**
 * Publishes the effective keymap (called only by the effective model,
 * `src/lib/actions/store.ts`); `null` reverts to the defaults. Notifies
 * `subscribeKeymap` listeners.
 */
export function setEffectiveKeymap(entries: KeymapEntry[] | null): void {
	effectiveKeymap = entries;
	for (const listener of [...keymapListeners]) {
		try {
			listener();
		} catch (err) {
			console.warn('[keymap] keymap listener failed:', err);
		}
	}
}

/** Called after every `setEffectiveKeymap()`. Returns an unsubscribe. */
export function subscribeKeymap(listener: () => void): () => void {
	keymapListeners.add(listener);
	return () => {
		keymapListeners.delete(listener);
	};
}

export type KeymapPlatform = 'mac' | 'other';

function livePlatform(): KeymapPlatform {
	return isMacPlatform() ? 'mac' : 'other';
}

/** The entries that apply on `platform` (G-ACTIONS §2.2 step 1). */
export function entriesForPlatform(entries: readonly KeymapEntry[], platform: KeymapPlatform): KeymapEntry[] {
	return entries.filter((e) => !e.platformOnly || e.platformOnly === platform);
}

/**
 * Hosted commands (§4.6): their keys live in the registry (rebindable,
 * visible to `conflicts()`), but their owner's own listener fires them — the
 * frame dispatcher and `useKey()` never do. `terminal.*` is hosted by the
 * xterm hook (DEC-56); the three Companion dispatch keys by the dispatch
 * input.
 */
export const HOSTED_COMMANDS: ReadonlySet<string> = new Set([
	'companion.send',
	'companion.new-run',
	'companion.persistent-run',
]);

export function isHostedCommand(command: string): boolean {
	return command.startsWith('terminal.') || HOSTED_COMMANDS.has(command);
}

/**
 * Looks up `command`'s registry entry. Some commands (`terminal.clear`,
 * `os.summon`) have one entry per platform family because the real binding
 * differs by platform, not just by how `mod` resolves — so this prefers the
 * entry that matches the caller's platform (`opts.mac`, defaulting to the
 * live platform), then (when `opts.scope` is given) the entry in that scope,
 * and only falls back to the first entry for a command with no
 * platform-matching one.
 */
export function findEntry(
	command: string,
	opts?: { mac?: boolean; scope?: KeymapScope; entries?: readonly KeymapEntry[] }
): KeymapEntry | undefined {
	const mac = opts?.mac ?? isMacPlatform();
	const platform: KeymapPlatform = mac ? 'mac' : 'other';
	let candidates = (opts?.entries ?? getKeymap()).filter((e) => e.command === command);
	const scope = opts?.scope;
	if (scope) {
		const scoped = candidates.filter((e) => (e.scope ?? 'app') === scope);
		if (scoped.length > 0) candidates = scoped;
	}
	return candidates.find((e) => !e.platformOnly || e.platformOnly === platform) ?? candidates[0];
}

/** Human-readable key hint for `command` — ⌘ on macOS, spelled-out Ctrl/Alt/
 *  Shift elsewhere. Returns `''` for an unknown command rather than throwing,
 *  since a stale command id in a caller shouldn't take down the tooltip. */
export function labelFor(command: string, opts?: { mac?: boolean }): string {
	const entry = findEntry(command, opts);
	if (!entry) return '';
	return formatKeyLabel(entry.key, opts);
}

// ─── Conflicts (DEC-59, G-ACTIONS §5) ──────────────────────────────────────

/** One same-key pair. `key` is the platform-resolved sequence; `whenA` /
 *  `whenB` are the **normalized** `when`s (`''` = always). */
export interface KeymapConflictPair {
	key: string;
	a: KeymapEntry;
	b: KeymapEntry;
	whenA: string;
	whenB: string;
	/** `when` — same key, different normalized `when` (resolved by layer,
	 *  then specificity, §2.3); `os-over-app` — an OS-wide key also bound
	 *  in-app, the OS rule (`a`) wins because the OS captures the key first;
	 *  `clash` — same key, same normalized `when`, same scope. */
	kind: 'clash' | 'when' | 'os-over-app';
}

export interface KeymapConflicts {
	/** Same key + same normalized `when` + same scope, different commands.
	 *  Reported (Keys tab conflict state), still resolved deterministically. */
	clashes: KeymapConflictPair[];
	/** Same key, different normalized `when` (or OS vs app). Shown, never an
	 *  error. */
	precedence: KeymapConflictPair[];
}

/** `shift+/` and `shift+=` name the same physical key as the shipped `?` /
 *  `plus` spellings (§3.1's one exception — `strokesFromEvent` matches both
 *  spellings to one event). `conflicts()` canonicalizes a resolved stroke to
 *  that shared spelling before grouping by key, so `?` vs `shift+/` (and
 *  `mod+plus` vs `mod+shift+=`) still group together instead of silently
 *  missing a same-`when` clash. */
function canonicalizeConflictStroke(stroke: string): string {
	const parts = stroke.split('+');
	const key = parts[parts.length - 1];
	const mods = parts.slice(0, -1);
	if (!mods.includes('shift')) return stroke;
	const rest = mods.filter((m) => m !== 'shift');
	if (key === '/') return [...rest, '?'].join('+');
	if (key === '=') return [...rest, 'plus'].join('+');
	return stroke;
}

function canonicalizeConflictKey(resolvedKeySequence: string): string {
	return resolvedKeySequence.split(' ').map(canonicalizeConflictStroke).join(' ');
}

/**
 * The comparison form of a key sequence on one platform: canonical modifier
 * order, `mod` resolved, and the §3.1 `?` / `plus` spellings folded — the
 * key `conflicts()` groups by, and the one the effective merge matches
 * negative rules and package key holds on (G-ACTIONS §1.5, §7.4).
 */
export function comparableKeySequence(seq: string, platform: KeymapPlatform): string {
	return canonicalizeConflictKey(resolveKeySequence(canonicalizeKeySequence(seq), platform === 'mac'));
}

function safeNormalize(when: string | undefined): string {
	try {
		return normalizeWhen(when);
	} catch {
		// An unparsable `when` never fires (`evaluateWhen`); it is compared by
		// its raw text so two identical broken rules still read as a clash.
		return `\u27e8invalid\u27e9 ${when ?? ''}`;
	}
}

/**
 * DEC-59's one conflict rule, per platform:
 * - a **clash** is two positive rules with the same platform-resolved key
 *   sequence and the same **normalized** `when` (never string equality of
 *   what was typed), bound to different commands, in the same scope; layer
 *   does not matter for detection;
 * - the same key with a different normalized `when` is **precedence**;
 * - OS scope is its own space: OS rules clash only with OS rules (they carry
 *   no `when`), and an OS key also bound in-app is precedence with the OS
 *   rule winning;
 * - a chord and a single stroke sharing a first stroke are neither (they are
 *   different sequences).
 *
 * `opts.entries` overrides the keymap it reads — the seam the tests (and the
 * Keys tab, over the effective keymap) use.
 */
export function conflicts(opts?: {
	platform?: KeymapPlatform;
	entries?: readonly KeymapEntry[];
}): KeymapConflicts {
	const platform = opts?.platform ?? livePlatform();
	const mac = platform === 'mac';
	const relevant = entriesForPlatform(opts?.entries ?? getKeymap(), platform);

	const byKey = new Map<string, Array<{ entry: KeymapEntry; when: string; scope: KeymapScope }>>();
	for (const entry of relevant) {
		const key = canonicalizeConflictKey(resolveKeySequence(entry.key, mac));
		const scope = entry.scope ?? 'app';
		// OS rules ignore focus: their `when` is always TRUE (§6, `E_OS_WHEN`).
		const when = scope === 'os' ? '' : safeNormalize(entry.when);
		const list = byKey.get(key);
		const row = { entry, when, scope };
		if (list) list.push(row);
		else byKey.set(key, [row]);
	}

	const out: KeymapConflicts = { clashes: [], precedence: [] };
	for (const [key, rows] of byKey) {
		for (let i = 0; i < rows.length; i++) {
			for (let j = i + 1; j < rows.length; j++) {
				let a = rows[i];
				let b = rows[j];
				if (a.entry.command === b.entry.command) continue;
				if (a.scope !== b.scope) {
					if (a.scope !== 'os') [a, b] = [b, a];
					out.precedence.push({ key, a: a.entry, b: b.entry, whenA: a.when, whenB: b.when, kind: 'os-over-app' });
					continue;
				}
				const pair = { key, a: a.entry, b: b.entry, whenA: a.when, whenB: b.when };
				if (a.when === b.when) out.clashes.push({ ...pair, kind: 'clash' });
				else out.precedence.push({ ...pair, kind: 'when' });
			}
		}
	}
	return out;
}

// ─── Keypress resolution (G-ACTIONS §2.3, DEC-58) ─────────────────────────

/** An entry of the effective keymap (`getKeymap()`): same shape as a
 *  `KeymapEntry`, named for G-ACTIONS-API. */
export type EffectiveKeymapEntry = KeymapEntry;

/** §2.3: higher layer wins. */
export const LAYER_RANK: Readonly<Record<KeymapSource, number>> = {
	default: 0,
	package: 1,
	personal: 2,
	project: 3,
};

function safeSpecificity(when: string | undefined | null): number {
	try {
		return whenSpecificity(when);
	} catch {
		return 0;
	}
}

/**
 * §2.3 winner among the rules that match a keypress (key sequence matched,
 * `when` true now, hosted commands already excluded): highest layer, then
 * the most specific `when`, then the latest in merge order. `keymap` is the
 * effective list the candidates come from (merge order = index). Exactly
 * one wins; nothing double-fires (DEC-58).
 */
export function resolveKeypressWinner(
	candidates: readonly KeymapEntry[],
	keymap: readonly KeymapEntry[]
): KeymapEntry | null {
	let best: KeymapEntry | null = null;
	let bestRank: [number, number, number] = [-1, -1, -1];
	for (const entry of candidates) {
		const rank: [number, number, number] = [
			LAYER_RANK[entry.source] ?? 0,
			safeSpecificity(entry.when),
			keymap.indexOf(entry),
		];
		if (
			!best ||
			rank[0] > bestRank[0] ||
			(rank[0] === bestRank[0] && rank[1] > bestRank[1]) ||
			(rank[0] === bestRank[0] && rank[1] === bestRank[1] && rank[2] > bestRank[2])
		) {
			best = entry;
			bestRank = rank;
		}
	}
	return best;
}

export interface KeypressResolution {
	/** The one command that fires (§2.3), or null. */
	winner: EffectiveKeymapEntry | null;
	/** Every in-app, non-hosted binding matching the key whose `when` holds. */
	candidates: EffectiveKeymapEntry[];
}

function isKeyboardEventLike(input: KeyboardEvent | { key: string }): input is KeyboardEvent {
	return 'ctrlKey' in input || 'metaKey' in input;
}

/**
 * The one "winner for a key + context" query (G-ACTIONS §2.3). `input` is a
 * keydown (matched like the dispatcher: single strokes only, IME / Dead-key
 * events match nothing) or a key string (`{ key: 'mod+b' }`, compared in
 * the platform-resolved form; chords allowed). Candidates are the effective
 * `scope: 'app'` entries on `platform` that match, are not hosted (§4.6)
 * and whose `when` is true against `ctx` (default: the live context of the
 * event's target). `entries` overrides the keymap (tests, previews).
 */
export function resolveKeypress(
	input: KeyboardEvent | { key: string },
	ctx?: ContextKeys,
	platform?: KeymapPlatform,
	entries?: readonly EffectiveKeymapEntry[]
): KeypressResolution {
	const p = platform ?? livePlatform();
	const mac = p === 'mac';
	const keymap = entries ?? getKeymap();
	const event = isKeyboardEventLike(input) ? input : null;
	const target = event ? null : comparableOrNull(input.key, p);
	if (!event && target === null) return { winner: null, candidates: [] };
	const context = ctx ?? getContextKeys(event?.target ?? null);
	const evalOptions = getEvalOptions();
	const candidates = entriesForPlatform(keymap, p).filter((entry) => {
		if ((entry.scope ?? 'app') !== 'app' || isHostedCommand(entry.command)) return false;
		const matches = event
			? eventMatchesCombo(event, entry.key, mac)
			: comparableOrNull(entry.key, p) === target;
		return matches && evaluateWhen(entry.when, context, evalOptions);
	});
	return { winner: resolveKeypressWinner(candidates, keymap), candidates };
}

function comparableOrNull(seq: string, platform: KeymapPlatform): string | null {
	try {
		return comparableKeySequence(seq, platform);
	} catch {
		return null;
	}
}

/**
 * Fire `handler` when `command`'s bound key is pressed and its `when` holds
 * against the live context (`context-keys.ts`). Centralises the guard every
 * frame shortcut needs (D3): a `!inputFocus` rule never fires while an
 * `input` / `textarea` / `contenteditable` holds focus, and an IME
 * composition or Dead-key event never matches (`strokesFromEvent`). No-ops
 * (and warns) for a command with no registry entry, and no-ops silently for
 * a hosted command (§4.6) — its owner fires it. Single strokes only; chords
 * go through the dispatcher's chord machine (WP-54, `chord.ts`). Every
 * keydown is resolved with `resolveKeypress()` over the effective keymap,
 * and the handler fires only when `command` is the §2.3 winner — so two
 * commands on one key never both fire (DEC-58), and a rebind takes effect
 * without remounting.
 */
export function useKey(
	command: string,
	handler: (e: KeyboardEvent) => void,
	opts?: { enabled?: boolean }
): void {
	const enabled = opts?.enabled ?? true;
	useEffect(() => {
		if (!enabled) return;
		if (isHostedCommand(command)) return;
		if (!getKeymap().some((e) => e.command === command) && !DEFAULT_KEYMAP.some((e) => e.command === command)) {
			console.warn(`[keymap] useKey: no registry entry for "${command}"`);
			return;
		}
		function onKey(e: KeyboardEvent) {
			const { winner } = resolveKeypress(e);
			if (!winner || winner.command !== command) return;
			e.preventDefault();
			handler(e);
		}
		window.addEventListener('keydown', onKey);
		return () => window.removeEventListener('keydown', onKey);
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [command, handler, enabled]);
}

/**
 * Read-only snapshot of the active keymap — the seam WP-21's `iyke keys
 * list` reads through (re-exported from `src/lib/iyke/keymap-bridge.ts`).
 */
export function listKeymap(): KeymapEntry[] {
	return getKeymap();
}
