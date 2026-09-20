// The one keymap registry (§6A.5): `{ command, key, when, source }`. Every
// consumer that shows or fires a key binding — the rail, the command
// palette, pane chrome, the native menu, and (later) the Shortcuts view and
// the Keys editor — reads through here instead of hard-coding a label.
//
// Phase 1 ships defaults only (`DEFAULT_KEYMAP`); user (`~/.ikenga/keybindings.json`)
// and project (`<project>/.ikenga/keybindings.json`) overrides are Phase 6.
// `getKeymap()` is already the seam a later phase merges onto — callers never
// read `DEFAULT_KEYMAP` directly.

import { useEffect } from 'react';
import { DEFAULT_KEYMAP, type KeymapEntry } from './defaults';
import { eventMatchesCombo, formatKeyLabel, isMacPlatform, resolveCombo } from './platform';
import { evaluateWhen, type WhenClause } from './when';

export type { KeymapEntry } from './defaults';
export type { WhenClause } from './when';
export { isTypingTarget } from './when';

/** The active keymap. Phase 1: defaults only — the merge point for Phase 6's
 *  user/project overrides. */
export function getKeymap(): KeymapEntry[] {
	return DEFAULT_KEYMAP;
}

/**
 * Looks up `command`'s registry entry. Some commands (`terminal.clear`) have
 * two entries gated by `platformOnly` because the real binding differs by
 * platform, not just by how `mod` resolves — so this prefers the entry that
 * matches the caller's platform (`opts.mac`, defaulting to the live
 * platform) and only falls back to the first entry for a command with no
 * platform-matching one.
 */
export function findEntry(command: string, opts?: { mac?: boolean }): KeymapEntry | undefined {
	const mac = opts?.mac ?? isMacPlatform();
	const platform = mac ? 'mac' : 'other';
	const candidates = getKeymap().filter((e) => e.command === command);
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

export interface KeymapConflict {
	key: string;
	when: string;
	commands: string[];
}

/**
 * Two `when` clauses "overlap" when a single keypress can satisfy both —
 * which is what makes two same-key bindings a real clash instead of
 * precedence. `global` always evaluates true, so it overlaps every clause,
 * including another `global`. The one clause pair that's genuinely mutually
 * exclusive is `not-input` / `terminal-focus`: the terminal's own input
 * surface is itself a typing target, so whichever one applies, the other
 * cannot (§6A.5's documented ⌘K palette-vs-terminal-clear precedence, the
 * motivating example). Any other same-key pairing (including same-clause
 * pairs) overlaps and is a candidate clash.
 */
function whenClausesOverlap(a: WhenClause, b: WhenClause): boolean {
	if (a === b) return true;
	const pair = [a, b].sort().join('|');
	return pair !== 'not-input|terminal-focus';
}

/** True when either entry lists the other in `knownOverlap` — a documented,
 *  shipped parallel-fire `conflicts()` should not report (see `defaults.ts`
 *  for what's declared and why). */
function isDocumentedOverlap(a: KeymapEntry, b: KeymapEntry): boolean {
	return Boolean(a.knownOverlap?.includes(b.command) || b.knownOverlap?.includes(a.command));
}

/**
 * A clash is two entries that resolve to the same physical key combo on a
 * given platform, with `when` clauses that overlap (see above), and that
 * aren't a documented parallel-fire pair (`knownOverlap`). Bindings tagged
 * `platformOnly` are only considered on that platform (the native menu only
 * installs on macOS; `terminal.clear`'s non-mac chord only applies there).
 *
 * `opts.entries` overrides the keymap `conflicts()` groups — the seam
 * `registry.test.ts` uses to prove the detector against a synthetic clash
 * without mutating the real `DEFAULT_KEYMAP` export.
 */
export function conflicts(opts?: {
	platform?: 'mac' | 'other';
	entries?: KeymapEntry[];
}): KeymapConflict[] {
	const platform = opts?.platform ?? (isMacPlatform() ? 'mac' : 'other');
	const mac = platform === 'mac';
	const relevant = (opts?.entries ?? getKeymap()).filter(
		(e) => !e.platformOnly || e.platformOnly === platform
	);

	const byKey = new Map<string, KeymapEntry[]>();
	for (const entry of relevant) {
		const combo = resolveCombo(entry.key, mac);
		const list = byKey.get(combo);
		if (list) list.push(entry);
		else byKey.set(combo, [entry]);
	}

	const out: KeymapConflict[] = [];
	for (const [combo, entries] of byKey) {
		if (entries.length <= 1) continue;

		// Union-find over entries that actually clash — two entries with the
		// same key can share a group transitively even if not every pair in
		// the group clashes directly, mirroring how a real double-fire reads.
		const parent = entries.map((_, i) => i);
		function find(i: number): number {
			while (parent[i] !== i) {
				parent[i] = parent[parent[i]];
				i = parent[i];
			}
			return i;
		}
		function union(i: number, j: number) {
			const ri = find(i);
			const rj = find(j);
			if (ri !== rj) parent[ri] = rj;
		}
		for (let i = 0; i < entries.length; i++) {
			for (let j = i + 1; j < entries.length; j++) {
				if (!whenClausesOverlap(entries[i].when, entries[j].when)) continue;
				if (isDocumentedOverlap(entries[i], entries[j])) continue;
				union(i, j);
			}
		}

		const groups = new Map<number, KeymapEntry[]>();
		entries.forEach((e, i) => {
			const root = find(i);
			const list = groups.get(root);
			if (list) list.push(e);
			else groups.set(root, [e]);
		});
		for (const group of groups.values()) {
			if (group.length <= 1) continue;
			out.push({
				key: combo,
				when: Array.from(new Set(group.map((e) => e.when))).join('|'),
				commands: group.map((e) => e.command),
			});
		}
	}
	return out;
}

/**
 * Fire `handler` when `command`'s bound key is pressed and its `when` clause
 * holds. Centralises the guard every frame shortcut needs (D3): a clause of
 * `not-input` never fires while an `input` / `textarea` / `contenteditable`
 * holds focus. No-ops (and warns) for a command with no registry entry, and
 * no-ops silently for a `terminal-focus` command — that clause is
 * descriptive only (see `when.ts`); it never gets a live listener here.
 */
export function useKey(
	command: string,
	handler: (e: KeyboardEvent) => void,
	opts?: { enabled?: boolean }
): void {
	const enabled = opts?.enabled ?? true;
	useEffect(() => {
		if (!enabled) return;
		const entry = findEntry(command);
		if (!entry) {
			console.warn(`[keymap] useKey: no registry entry for "${command}"`);
			return;
		}
		if (entry.when === 'terminal-focus') return;
		const mac = isMacPlatform();
		function onKey(e: KeyboardEvent) {
			if (!entry) return;
			if (!eventMatchesCombo(e, entry.key, mac)) return;
			if (!evaluateWhen(entry.when, e)) return;
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
