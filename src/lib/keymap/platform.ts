// Key-combo parsing, matching, and display formatting. Grammar: modifiers
// joined by `+`, lowercase, in any order, with the physical key last —
// `mod+1`, `mod+shift+n`, `ctrl+t`. `mod` is the platform-primary modifier
// (⌘ on macOS, Ctrl elsewhere, per §2 of the interaction spec); `ctrl` is the
// *literal* Control key regardless of platform (the spec's `⌃` bindings —
// `⌃T`, `⌃1`..`⌃6` — are deliberately not platform-swapped). `meta` is a
// literal Cmd/Win key, for the rare binding that needs one specifically.

import { isMac as isMacDefault } from '@/lib/platform';

export function isMacPlatform(): boolean {
	return isMacDefault;
}

interface ParsedCombo {
	mods: Set<string>;
	key: string;
}

export function parseCombo(combo: string): ParsedCombo {
	const parts = combo
		.split('+')
		.map((p) => p.trim().toLowerCase())
		.filter((p) => p.length > 0);
	const key = parts[parts.length - 1] ?? '';
	const mods = new Set(parts.slice(0, -1));
	return { mods, key };
}

function resolvedModifiers(mods: Set<string>, mac: boolean) {
	const usesMod = mods.has('mod');
	return {
		meta: usesMod ? mac : mods.has('meta'),
		ctrl: usesMod ? !mac : mods.has('ctrl'),
		shift: mods.has('shift'),
		alt: mods.has('alt'),
	};
}

/** Canonical, platform-resolved signature for a combo — used by `conflicts()`
 *  to group bindings that land on the same physical keys on a given
 *  platform, even when they're spelled differently (`mod+t` vs `ctrl+t`). */
export function resolveCombo(combo: string, mac: boolean): string {
	const { mods, key } = parseCombo(combo);
	const { meta, ctrl, alt, shift } = resolvedModifiers(mods, mac);
	return [meta && 'meta', ctrl && 'ctrl', alt && 'alt', shift && 'shift', key]
		.filter((v): v is string => Boolean(v))
		.join('+');
}

/** Does `e` match `combo` on this platform? */
export function eventMatchesCombo(
	e: Pick<KeyboardEvent, 'key' | 'metaKey' | 'ctrlKey' | 'shiftKey' | 'altKey'>,
	combo: string,
	mac: boolean
): boolean {
	const { mods, key } = parseCombo(combo);
	if (e.key.toLowerCase() !== key) return false;
	const { meta, ctrl, alt, shift } = resolvedModifiers(mods, mac);
	return e.metaKey === meta && e.ctrlKey === ctrl && e.shiftKey === shift && e.altKey === alt;
}

const NAMED_KEYS: Record<string, string> = {
	enter: '⏎',
	escape: 'Esc',
	backspace: '⌫',
	tab: 'Tab',
};

function keyGlyph(key: string): string {
	if (key.length === 1) return key.toUpperCase();
	return NAMED_KEYS[key] ?? key.charAt(0).toUpperCase() + key.slice(1);
}

/** Human-readable label for a combo — `labelFor()`'s formatting engine.
 *  macOS concatenates symbols with no separator (⌘⇧T, matching every glyph
 *  already shipped in the codebase); other platforms spell modifiers out
 *  (Ctrl+Shift+T), since there's no single-glyph convention for them here. */
export function formatKeyLabel(combo: string, opts?: { mac?: boolean }): string {
	const mac = opts?.mac ?? isMacPlatform();
	const { mods, key } = parseCombo(combo);
	const { meta, ctrl, alt, shift } = resolvedModifiers(mods, mac);
	const glyph = keyGlyph(key);
	if (mac) {
		// Matches the glyph order already shipped across the codebase
		// (`⌃⇧T`, `⌘⇧N`, `⌘⇧\`): primary modifier (⌃ or ⌘) first, then ⌥, then ⇧.
		let out = '';
		if (ctrl) out += '⌃';
		if (meta) out += '⌘';
		if (alt) out += '⌥';
		if (shift) out += '⇧';
		return out + glyph;
	}
	const parts: string[] = [];
	if (ctrl || meta) parts.push('Ctrl');
	if (alt) parts.push('Alt');
	if (shift) parts.push('Shift');
	parts.push(glyph);
	return parts.join('+');
}

/** Tauri `accelerator` syntax (`CmdOrCtrl+Shift+X`) for native-menu items —
 *  the same combo grammar, formatted for the platform Menu API instead of
 *  for display. */
export function toAccelerator(combo: string): string {
	const { mods, key } = parseCombo(combo);
	const parts: string[] = [];
	if (mods.has('mod')) parts.push('CmdOrCtrl');
	if (mods.has('ctrl') && !mods.has('mod')) parts.push('Ctrl');
	if (mods.has('meta') && !mods.has('mod')) parts.push('Cmd');
	if (mods.has('alt')) parts.push('Alt');
	if (mods.has('shift')) parts.push('Shift');
	parts.push(key.length === 1 ? key.toUpperCase() : key);
	return parts.join('+');
}
