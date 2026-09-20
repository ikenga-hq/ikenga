import { act, renderHook } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { listKeymap as bridgeListKeymap } from '@/lib/iyke/keymap-bridge';
import { DEFAULT_KEYMAP, type KeymapEntry } from './defaults';
import { conflicts, findEntry, getKeymap, labelFor, listKeymap, useKey } from './registry';
import { isMacPlatform } from './platform';

function fireKeydown(init: KeyboardEventInit, target?: EventTarget) {
	const event = new KeyboardEvent('keydown', { ...init, cancelable: true, bubbles: true });
	(target ?? window).dispatchEvent(event);
	return event;
}

// `rail.project` is bound to `mod+1` — resolve the actual modifier for whichever
// platform this test process reports, so the test doesn't hard-code an
// assumption about which OS it runs on.
const MOD_KEY_INIT: KeyboardEventInit = isMacPlatform()
	? { key: '1', metaKey: true }
	: { key: '1', ctrlKey: true };
const UNRELATED_KEY_INIT: KeyboardEventInit = isMacPlatform()
	? { key: '2', metaKey: true }
	: { key: '2', ctrlKey: true };

describe('conflicts()', () => {
	it('returns empty for the shipped defaults on macOS', () => {
		expect(conflicts({ platform: 'mac' })).toEqual([]);
	});

	it('returns empty for the shipped defaults on non-macOS', () => {
		expect(conflicts({ platform: 'other' })).toEqual([]);
	});

	it('treats same key + different `when` as precedence, not a clash', () => {
		// palette.open (not-input) and terminal.clear's mac entry
		// (terminal-focus) both bind mod+k — documented precedence per
		// §6A.5, never a conflict. Pass `mac: true` explicitly: terminal.clear
		// has a second, non-mac entry (ctrl+shift+k) that findEntry() would
		// otherwise return on a non-mac test runner.
		const paletteOpen = findEntry('palette.open', { mac: true });
		const terminalClear = findEntry('terminal.clear', { mac: true });
		expect(paletteOpen?.key).toBe('mod+k');
		expect(terminalClear?.key).toBe('mod+k');
		expect(paletteOpen?.when).not.toBe(terminalClear?.when);
		expect(conflicts({ platform: 'mac' })).toEqual([]);
	});

	it('flags a real clash — same resolved key AND same `when`', () => {
		// Not a real scenario in DEFAULT_KEYMAP; prove the detector — the real
		// `conflicts()`, via its `entries` override — actually detects
		// something rather than vacuously returning [] always.
		const original = getKeymap();
		expect(original.length).toBeGreaterThan(0);
		const withClash: KeymapEntry[] = [
			...DEFAULT_KEYMAP,
			{
				command: 'test.clash',
				key: 'mod+1',
				when: 'not-input',
				source: 'default',
				label: 'Test clash',
			},
		];
		const result = conflicts({ platform: 'mac', entries: withClash });
		expect(result.length).toBe(1);
		expect(result[0].commands.sort()).toEqual(['rail.project', 'test.clash']);
	});

	it('resolves the documented ⌘T/⌃T non-mac collision via platformOnly, not omission', () => {
		// palette.views (mod+t) and pane.reopen (mod+shift+t) are both
		// platformOnly: 'mac' precisely because on non-mac `mod` resolves to
		// the literal Ctrl key, landing on the same combo as
		// pane.new-shell-terminal (ctrl+t) / pane.new-claude-terminal
		// (ctrl+shift+t) — a real clash the shipped app avoids only because
		// the terminal's `ctrlOnly` branch runs first and always wins
		// (workspace.tsx, §2). Prove both entries are present (excluded from
		// non-mac `conflicts()` deliberately, not silently missing from the
		// keymap) and that both platforms come back clean.
		const paletteViews = findEntry('palette.views');
		const paneReopen = findEntry('pane.reopen');
		expect(paletteViews?.platformOnly).toBe('mac');
		expect(paneReopen?.platformOnly).toBe('mac');
		expect(conflicts({ platform: 'other' })).toEqual([]);
		expect(conflicts({ platform: 'mac' })).toEqual([]);
	});

	it('treats a mac-only native-menu accelerator as `global`, not `not-input`', () => {
		// menu.new-terminal (⌘T) is an OS Menu accelerator — it fires
		// regardless of DOM focus, unlike palette.views (also ⌘T) which is a
		// real in-app `not-input` listener. `global` overlaps `not-input`, so
		// this pair only stays out of `conflicts()` because it's declared via
		// `knownOverlap` below — not because `when` hides the overlap.
		const menuNewTerminal = findEntry('menu.new-terminal');
		expect(menuNewTerminal?.when).toBe('global');
		expect(conflicts({ platform: 'mac' })).toEqual([]);
	});

	it('documents the mac ⌘T menu/palette double-fire via `knownOverlap`, not by hiding it', () => {
		const menuNewTerminal = findEntry('menu.new-terminal');
		const paletteViews = findEntry('palette.views');
		expect(menuNewTerminal?.knownOverlap).toContain('palette.views');
		expect(paletteViews).toBeDefined();
		// Removing the declaration (simulated here via the `entries` override)
		// makes the real detector report it — proving the empty result above
		// comes from the documentation, not from an undetectable pairing.
		const withoutDeclaration = getKeymap().map((e) =>
			e.command === 'menu.new-terminal' ? { ...e, knownOverlap: undefined } : e
		);
		const result = conflicts({ platform: 'mac', entries: withoutDeclaration });
		const group = result.find((c) => c.commands.includes('menu.new-terminal'));
		expect(group?.commands.sort()).toEqual(['menu.new-terminal', 'palette.views']);
	});

	it('documents the non-mac rail/pane-focus double-fire via `knownOverlap`, not by hiding it', () => {
		// On non-mac, `mod+1` (rail.project) resolves to the literal `ctrl+1` that
		// `pane.focus-1` (global) is also bound to — both are live listeners
		// with no stopPropagation, so today's app really does double-fire.
		const railProject = findEntry('rail.project');
		expect(railProject?.knownOverlap).toContain('pane.focus-1');
		expect(conflicts({ platform: 'other' })).toEqual([]);

		const withoutDeclaration = getKeymap().map((e) =>
			e.command === 'rail.project' ? { ...e, knownOverlap: undefined } : e
		);
		const result = conflicts({ platform: 'other', entries: withoutDeclaration });
		const group = result.find((c) => c.commands.includes('rail.project'));
		expect(group?.commands.sort()).toEqual(['pane.focus-1', 'rail.project']);
	});

	it('reports an undocumented same-key `global` + `not-input` pair as a real clash', () => {
		// palette.projects (mod+p, not-input) with no declared knownOverlap —
		// a synthetic `global` binding on the same key must be flagged, since
		// `global` overlaps `not-input` and nothing documents it away.
		const withOverlap: KeymapEntry[] = [
			...DEFAULT_KEYMAP,
			{
				command: 'test.global-clash',
				key: 'mod+p',
				when: 'global',
				source: 'default',
				label: 'Test global clash',
			},
		];
		const result = conflicts({ platform: 'mac', entries: withOverlap });
		const group = result.find((c) => c.commands.includes('test.global-clash'));
		expect(group?.commands.sort()).toEqual(['palette.projects', 'test.global-clash']);
	});

	it('does not flag same-key entries whose `when` is mutually exclusive (not-input vs terminal-focus)', () => {
		const withPrecedence: KeymapEntry[] = [
			...DEFAULT_KEYMAP,
			{
				command: 'test.terminal-only',
				key: 'mod+p',
				when: 'terminal-focus',
				source: 'default',
				label: 'Test terminal-only',
			},
		];
		const result = conflicts({ platform: 'mac', entries: withPrecedence });
		expect(result.find((c) => c.commands.includes('test.terminal-only'))).toBeUndefined();
	});
});

describe('labelFor()', () => {
	it('renders the mac glyph form', () => {
		expect(labelFor('rail.project', { mac: true })).toBe('⌘1');
		expect(labelFor('pane.split-down', { mac: true })).toBe('⌘⇧\\');
	});

	it('renders the spelled-out form elsewhere', () => {
		expect(labelFor('rail.project', { mac: false })).toBe('Ctrl+1');
		expect(labelFor('pane.split-down', { mac: false })).toBe('Ctrl+Shift+\\');
	});

	it('keeps the literal ctrl modifier distinct from mod on every platform', () => {
		expect(labelFor('pane.new-shell-terminal', { mac: true })).toBe('⌃T');
		expect(labelFor('pane.new-shell-terminal', { mac: false })).toBe('Ctrl+T');
	});

	it('returns "" for an unknown command instead of throwing', () => {
		expect(labelFor('nope.does-not-exist')).toBe('');
	});

	it('picks the platform-matching entry for a command with two platformOnly variants', () => {
		// terminal.clear has two entries because the real terminal default
		// differs by platform (Cmd+K on mac, Ctrl+Shift+K elsewhere) — not
		// just how `mod` resolves — so findEntry()/labelFor() must not
		// silently return the first-declared (mac) entry on non-mac.
		expect(labelFor('terminal.clear', { mac: true })).toBe('⌘K');
		expect(labelFor('terminal.clear', { mac: false })).toBe('Ctrl+Shift+K');
	});
});

describe('useKey() — D3: typing in input/textarea/contenteditable never fires a frame shortcut', () => {
	it('does not fire when the event target is an <input>', () => {
		const handler = vi.fn();
		renderHook(() => useKey('rail.project', handler));
		const input = document.createElement('input');
		document.body.appendChild(input);
		act(() => {
			fireKeydown(MOD_KEY_INIT, input);
		});
		expect(handler).not.toHaveBeenCalled();
		input.remove();
	});

	it('does not fire when the event target is a <textarea>', () => {
		const handler = vi.fn();
		renderHook(() => useKey('rail.project', handler));
		const textarea = document.createElement('textarea');
		document.body.appendChild(textarea);
		act(() => {
			fireKeydown(MOD_KEY_INIT, textarea);
		});
		expect(handler).not.toHaveBeenCalled();
		textarea.remove();
	});

	it('does not fire when the event target is contenteditable', () => {
		const handler = vi.fn();
		renderHook(() => useKey('rail.project', handler));
		const div = document.createElement('div');
		div.setAttribute('contenteditable', 'true');
		document.body.appendChild(div);
		act(() => {
			fireKeydown(MOD_KEY_INIT, div);
		});
		expect(handler).not.toHaveBeenCalled();
		div.remove();
	});

	it('fires for a non-input target with the bound combo', () => {
		const handler = vi.fn();
		renderHook(() => useKey('rail.project', handler));
		act(() => {
			fireKeydown(MOD_KEY_INIT, document.body);
		});
		expect(handler).toHaveBeenCalledTimes(1);
	});

	it('does not fire for an unrelated key', () => {
		const handler = vi.fn();
		renderHook(() => useKey('rail.project', handler));
		act(() => {
			fireKeydown(UNRELATED_KEY_INIT, document.body);
		});
		expect(handler).not.toHaveBeenCalled();
	});
});

describe('listKeymap() — D4: bridge exposure for iyke keys list', () => {
	it('returns the full registry', () => {
		expect(listKeymap()).toEqual(getKeymap());
		expect(listKeymap().length).toBe(DEFAULT_KEYMAP.length);
	});

	it('is re-exported unchanged from src/lib/iyke/keymap-bridge', () => {
		expect(bridgeListKeymap()).toEqual(listKeymap());
	});
});
