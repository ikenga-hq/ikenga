import { act, renderHook } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { listKeymap as bridgeListKeymap } from '@/lib/iyke/keymap-bridge';
import { DEFAULT_KEYMAP, type KeymapEntry } from './defaults';
import { conflicts, findEntry, getKeymap, isHostedCommand, labelFor, listKeymap, useKey } from './registry';
import { isMacPlatform, validateKeySequence } from './platform';
import { normalizeWhen } from './when';

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

const entry = (e: Partial<KeymapEntry> & Pick<KeymapEntry, 'command' | 'key'>): KeymapEntry => ({
	when: '!inputFocus',
	source: 'default',
	label: e.command,
	...e,
});

const pairCommands = (pairs: { a: KeymapEntry; b: KeymapEntry }[]) =>
	pairs.map((p) => [p.a.command, p.b.command].sort().join(' + ')).sort();

describe('conflicts() — DEC-59', () => {
	it('has no clashes for the shipped defaults on macOS', () => {
		expect(conflicts({ platform: 'mac' }).clashes).toEqual([]);
	});

	it('has no clashes for the shipped defaults on Windows/Linux', () => {
		expect(conflicts({ platform: 'other' }).clashes).toEqual([]);
	});

	it('lists the defaults\' same-key pairs as precedence, separately (macOS)', () => {
		// ⌘K palette vs hosted terminal clear; ⌘N Create vs the native New
		// Session accelerator; ⌘T views palette vs the native New Terminal
		// accelerator (the latter two are re-keyed by DEC-64 in WP-54).
		expect(pairCommands(conflicts({ platform: 'mac' }).precedence)).toEqual(
			['menu.new-session + ngwa.create', 'menu.new-terminal + palette.views', 'palette.open + terminal.clear'].sort()
		);
	});

	it('lists the defaults\' same-key pairs as precedence, separately (Windows/Linux)', () => {
		// Ctrl+1..3: rail (!inputFocus) vs pane focus (always) — DEC-64 moves
		// pane focus to Alt+1–6 in WP-54.
		expect(pairCommands(conflicts({ platform: 'other' }).precedence)).toEqual(
			['pane.focus-1 + rail.project', 'pane.focus-2 + rail.chi', 'pane.focus-3 + rail.ngwa'].sort()
		);
	});

	it('reports a synthetic clash: same resolved key AND same normalized `when`', () => {
		const withClash = [...DEFAULT_KEYMAP, entry({ command: 'test.clash', key: 'mod+1', when: '!inputFocus' })];
		const { clashes } = conflicts({ platform: 'mac', entries: withClash });
		expect(clashes).toHaveLength(1);
		expect([clashes[0].a.command, clashes[0].b.command].sort()).toEqual(['rail.project', 'test.clash']);
		expect(clashes[0].kind).toBe('clash');
		expect(clashes[0].key).toBe('meta+1');
		expect(clashes[0].whenA).toBe('!inputFocus');
	});

	it('compares normalized forms, never the typed string', () => {
		// `not-input` (legacy), `!inputFocus` and `!(inputFocus)` are one `when`.
		for (const when of ['not-input', '!(inputFocus)', '!inputFocus && always', '!!!inputFocus']) {
			const { clashes } = conflicts({
				platform: 'mac',
				entries: [entry({ command: 'a', key: 'mod+p' }), entry({ command: 'b', key: 'mod+p', when })],
			});
			expect(clashes, when).toHaveLength(1);
		}
		// Operand order does not matter either.
		const { clashes } = conflicts({
			platform: 'mac',
			entries: [
				entry({ command: 'a', key: 'mod+p', when: "filesFocus && resource =~ '*.ts'" }),
				entry({ command: 'b', key: 'mod+p', when: "resource =~ \"*.ts\" && filesFocus" }),
			],
		});
		expect(clashes).toHaveLength(1);
	});

	it('resolves `mod` before comparing: `mod+t` and `ctrl+t` clash only where they land on one key', () => {
		const entries = [
			entry({ command: 'a', key: 'mod+t', when: 'paneFocus' }),
			entry({ command: 'b', key: 'ctrl+t', when: 'paneFocus' }),
		];
		expect(conflicts({ platform: 'other', entries }).clashes).toHaveLength(1);
		expect(conflicts({ platform: 'mac', entries }).clashes).toHaveLength(0);
	});

	it('treats same key + different normalized `when` as precedence, never a clash', () => {
		const entries = [
			entry({ command: 'a', key: 'mod+p', when: 'always' }),
			entry({ command: 'b', key: 'mod+p', when: '!inputFocus' }),
			entry({ command: 'c', key: 'mod+p', when: 'terminalFocus' }),
		];
		const r = conflicts({ platform: 'mac', entries });
		expect(r.clashes).toEqual([]);
		expect(pairCommands(r.precedence)).toEqual(['a + b', 'a + c', 'b + c']);
		expect(r.precedence.every((p) => p.kind === 'when')).toBe(true);
	});

	it('does not apply De Morgan or distribution (syntactic normalization)', () => {
		const r = conflicts({
			platform: 'mac',
			entries: [
				entry({ command: 'a', key: 'mod+p', when: 'x && (y || z)' }),
				entry({ command: 'b', key: 'mod+p', when: '(x && y) || (x && z)' }),
			],
		});
		expect(r.clashes).toEqual([]);
		expect(r.precedence).toHaveLength(1);
	});

	it('a chord and a single stroke sharing a first stroke are neither', () => {
		const r = conflicts({
			platform: 'mac',
			entries: [entry({ command: 'palette.open', key: 'mod+k' }), entry({ command: 'release-status', key: 'mod+k mod+r' })],
		});
		expect(r).toEqual({ clashes: [], precedence: [] });
	});

	it('two chords with the same sequence and `when` clash', () => {
		const r = conflicts({
			platform: 'mac',
			entries: [entry({ command: 'a', key: 'mod+k mod+r' }), entry({ command: 'b', key: 'mod+k mod+r' })],
		});
		expect(r.clashes).toHaveLength(1);
		expect(r.clashes[0].key).toBe('meta+k meta+r');
	});

	it('treats OS scope as its own space: OS vs OS clash, OS vs app is precedence with the OS rule first', () => {
		const os1 = entry({ command: 'os.summon', key: 'alt+space', when: 'always', scope: 'os' });
		const os2 = entry({ command: 'os.other', key: 'alt+space', when: 'always', scope: 'os' });
		const app = entry({ command: 'app.thing', key: 'alt+space', when: 'always' });
		const clash = conflicts({ platform: 'mac', entries: [os1, os2] });
		expect(clash.clashes).toHaveLength(1);
		const prec = conflicts({ platform: 'mac', entries: [app, os1] });
		expect(prec.clashes).toEqual([]);
		expect(prec.precedence).toHaveLength(1);
		expect(prec.precedence[0].kind).toBe('os-over-app');
		expect(prec.precedence[0].a.command).toBe('os.summon');
	});

	it('only considers entries for the requested platform', () => {
		const entries = [
			entry({ command: 'a', key: 'mod+p', platformOnly: 'mac' }),
			entry({ command: 'b', key: 'mod+p', platformOnly: 'other' }),
		];
		expect(conflicts({ platform: 'mac', entries })).toEqual({ clashes: [], precedence: [] });
		expect(conflicts({ platform: 'other', entries })).toEqual({ clashes: [], precedence: [] });
	});

	it('ignores two entries for the same command', () => {
		const entries = [entry({ command: 'a', key: 'mod+p' }), entry({ command: 'a', key: 'mod+p' })];
		expect(conflicts({ platform: 'mac', entries })).toEqual({ clashes: [], precedence: [] });
	});

	it('no longer reads `knownOverlap` (the pairwise overlap table is gone)', () => {
		const stripped = getKeymap().map((e) => ({ ...e, knownOverlap: undefined }));
		expect(conflicts({ platform: 'mac', entries: stripped })).toEqual(conflicts({ platform: 'mac' }));
		expect(conflicts({ platform: 'other', entries: stripped })).toEqual(conflicts({ platform: 'other' }));
	});
});

describe('defaults — DEC-62 `when` values and the new fields', () => {
	it('every default `when` parses, and none uses a legacy word', () => {
		for (const e of DEFAULT_KEYMAP) {
			expect(() => normalizeWhen(e.when), e.command).not.toThrow();
			expect(['global', 'not-input', 'terminal-focus']).not.toContain(e.when);
		}
	});

	it('every default key passes the §3.1 grammar', () => {
		for (const e of DEFAULT_KEYMAP) expect(validateKeySequence(e.key), `${e.command} ${e.key}`).toBeNull();
	});

	it('carries the `source` and (defaulted) `scope` fields', () => {
		for (const e of DEFAULT_KEYMAP) {
			expect(e.source).toBe('default');
			expect(e.scope ?? 'app').toBe('app');
		}
	});

	it('marks the hosted commands', () => {
		expect(isHostedCommand('terminal.clear')).toBe(true);
		expect(isHostedCommand('companion.send')).toBe(true);
		expect(isHostedCommand('palette.open')).toBe(false);
	});
});

describe('findEntry()', () => {
	it('prefers the platform entry, then the requested scope', () => {
		const entries = [
			entry({ command: 'os.summon', key: 'alt+space', scope: 'os', platformOnly: 'mac' }),
			entry({ command: 'os.summon', key: 'meta+space', scope: 'os', platformOnly: 'other' }),
		];
		expect(findEntry('os.summon', { mac: true, entries })?.key).toBe('alt+space');
		expect(findEntry('os.summon', { mac: false, entries })?.key).toBe('meta+space');
		const mixed = [entry({ command: 'x', key: 'mod+1', scope: 'os' }), entry({ command: 'x', key: 'mod+2' })];
		expect(findEntry('x', { mac: true, entries: mixed, scope: 'app' })?.key).toBe('mod+2');
		expect(findEntry('x', { mac: true, entries: mixed, scope: 'os' })?.key).toBe('mod+1');
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

	it('never fires during IME composition (§3.3)', () => {
		const handler = vi.fn();
		renderHook(() => useKey('rail.project', handler));
		act(() => {
			fireKeydown({ ...MOD_KEY_INIT, isComposing: true }, document.body);
		});
		expect(handler).not.toHaveBeenCalled();
	});

	it('never installs a listener for a hosted command (§4.6)', () => {
		const handler = vi.fn();
		renderHook(() => useKey('companion.send', handler));
		act(() => {
			fireKeydown({ key: 'Enter' }, document.body);
		});
		expect(handler).not.toHaveBeenCalled();
	});

	it('fires an `always` binding even inside an input', () => {
		const handler = vi.fn();
		renderHook(() => useKey('explorer.toggle', handler));
		const input = document.createElement('input');
		document.body.appendChild(input);
		act(() => {
			fireKeydown(isMacPlatform() ? { key: 'b', metaKey: true } : { key: 'b', ctrlKey: true }, input);
		});
		expect(handler).toHaveBeenCalledTimes(1);
		input.remove();
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
