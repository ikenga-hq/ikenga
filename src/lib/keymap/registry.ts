// The one keymap registry (§6A.5): `{ command, key, when, source }`. Every
// consumer that shows or fires a key binding — the rail, the command
// palette, pane chrome, the native menu, and (later) the Shortcuts view and
// the Keys editor — reads through here instead of hard-coding a label.
//
// Keymap v2 (WP-49, G-ACTIONS §2–§5): `when` is a DEC-62 expression
// (`when.ts`) evaluated against the context-key service (`context-keys.ts`);
// `conflicts()` follows DEC-59 (normalized-`when` comparison, clash vs
// precedence, OS scope as its own space); chords are handled by `chord.ts`.
// `getKeymap()` returns the defaults until WP-52 merges the package /
// personal / project layers onto it — callers never read `DEFAULT_KEYMAP`
// directly.

import { useEffect } from 'react';
import { getContextKeys, getEvalOptions } from './context-keys';
import { DEFAULT_KEYMAP, type KeymapEntry, type KeymapScope } from './defaults';
import { eventMatchesCombo, formatKeyLabel, isMacPlatform, resolveKeySequence } from './platform';
import { evaluateWhen, normalizeWhen } from './when';

export type { KeymapEntry, KeymapScope, KeymapSource } from './defaults';
export type { WhenClause } from './when';
export { isTypingTarget } from './when';

/** The active keymap. Today: defaults only — WP-52 merges the package /
 *  personal / project layers here (callers never read `DEFAULT_KEYMAP`
 *  directly). */
export function getKeymap(): KeymapEntry[] {
	return DEFAULT_KEYMAP;
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
		const key = resolveKeySequence(entry.key, mac);
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

/**
 * Fire `handler` when `command`'s bound key is pressed and its `when` holds
 * against the live context (`context-keys.ts`). Centralises the guard every
 * frame shortcut needs (D3): a `!inputFocus` rule never fires while an
 * `input` / `textarea` / `contenteditable` holds focus, and an IME
 * composition or Dead-key event never matches (`strokesFromEvent`). No-ops
 * (and warns) for a command with no registry entry, and no-ops silently for
 * a hosted command (§4.6) — its owner fires it. Single strokes only; chords
 * go through the dispatcher's chord machine (WP-54, `chord.ts`).
 */
export function useKey(
	command: string,
	handler: (e: KeyboardEvent) => void,
	opts?: { enabled?: boolean }
): void {
	const enabled = opts?.enabled ?? true;
	useEffect(() => {
		if (!enabled) return;
		const entry = findEntry(command, { scope: 'app' });
		if (!entry) {
			console.warn(`[keymap] useKey: no registry entry for "${command}"`);
			return;
		}
		if (isHostedCommand(command) || (entry.scope ?? 'app') === 'os') return;
		const mac = isMacPlatform();
		function onKey(e: KeyboardEvent) {
			if (!entry) return;
			if (!eventMatchesCombo(e, entry.key, mac)) return;
			if (!evaluateWhen(entry.when, getContextKeys(e.target), getEvalOptions())) return;
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
