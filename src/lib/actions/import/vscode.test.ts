// WP-61 unit tests — written under DEC-50, not run this session (WP-63 runs
// them). Covers the JSONC reader, the key/`when` translators and the diff
// classifier against the DEFAULT-only keymap (this file never calls
// `startActionsStore()`, so `keyHolder()` reads the module's initial
// `EMPTY_MODEL` — the default layer only, exactly `DEFAULT_KEYMAP` merged —
// which is enough to exercise "free" vs "held" without a store harness).

import { describe, expect, it } from 'vitest';
import {
	buildVSCodeDiff,
	parseVSCodeKeybindingsText,
	translateVSCodeKey,
	translateVSCodeWhen,
} from './vscode';

describe('parseVSCodeKeybindingsText', () => {
	it('parses a plain JSON array', () => {
		const rows = parseVSCodeKeybindingsText('[{"key":"ctrl+b","command":"workbench.action.toggleSidebarVisibility"}]');
		expect(rows).toEqual([{ key: 'ctrl+b', command: 'workbench.action.toggleSidebarVisibility' }]);
	});

	it('strips line and block comments outside strings', () => {
		const text = `[
			// a leading comment
			{ "key": "ctrl+p", /* inline */ "command": "workbench.action.quickOpen" }
		]`;
		const rows = parseVSCodeKeybindingsText(text);
		expect(rows).toEqual([{ key: 'ctrl+p', command: 'workbench.action.quickOpen' }]);
	});

	it('keeps `//` inside a string value', () => {
		const rows = parseVSCodeKeybindingsText('[{"key":"a","command":"x","when":"resource =~ \'https://x\'"}]');
		expect(rows[0].when).toBe("resource =~ 'https://x'");
	});

	it('strips trailing commas before `}` and `]`', () => {
		const rows = parseVSCodeKeybindingsText('[{"key":"a","command":"b",},]');
		expect(rows).toEqual([{ key: 'a', command: 'b' }]);
	});

	it('throws on a non-array root', () => {
		expect(() => parseVSCodeKeybindingsText('{"key":"a"}')).toThrow(/top-level array/);
	});

	it('throws on invalid JSON', () => {
		expect(() => parseVSCodeKeybindingsText('[{')).toThrow();
	});
});

describe('translateVSCodeKey', () => {
	it('folds a single ctrl or cmd onto `mod`', () => {
		expect(translateVSCodeKey('ctrl+b')).toBe('mod+b');
		expect(translateVSCodeKey('cmd+shift+p')).toBe('mod+shift+p');
	});

	it('keeps ctrl+cmd literal when both are held', () => {
		expect(translateVSCodeKey('ctrl+cmd+p')).toBe('ctrl+meta+p');
	});

	it('translates a two-stroke chord', () => {
		expect(translateVSCodeKey('ctrl+k ctrl+r')).toBe('mod+k mod+r');
	});

	it('maps named keys (arrows, escape, …)', () => {
		expect(translateVSCodeKey('up')).toBe('arrowup');
		expect(translateVSCodeKey('ctrl+escape')).toBe('mod+escape');
	});

	it('returns null for an unrecognized modifier or key token', () => {
		expect(translateVSCodeKey('ctrl+numpad1')).toBeNull();
		expect(translateVSCodeKey('hyper+b')).toBeNull();
	});

	it('returns null for more than two strokes', () => {
		expect(translateVSCodeKey('ctrl+k ctrl+r ctrl+x')).toBeNull();
	});
});

describe('translateVSCodeWhen', () => {
	it('treats an empty/absent clause as always', () => {
		expect(translateVSCodeWhen(undefined)).toEqual({ ok: true, when: undefined });
		expect(translateVSCodeWhen('  ')).toEqual({ ok: true, when: undefined });
	});

	it('passes a recognized context key through', () => {
		const result = translateVSCodeWhen('terminalFocus');
		expect(result.ok).toBe(true);
		if (result.ok) expect(result.when).toBe('terminalFocus');
	});

	it('substitutes a mapped VS Code key onto its Ikenga equivalent', () => {
		const result = translateVSCodeWhen('editorTextFocus');
		expect(result.ok).toBe(true);
		if (result.ok) expect(result.when).toBe('inputFocus');
	});

	it('substitutes inside a negation', () => {
		const result = translateVSCodeWhen('!sideBarFocus');
		expect(result.ok).toBe(true);
		if (result.ok) expect(result.when).toContain('explorerFocus');
	});

	it('rejects a context key outside the supported subset', () => {
		const result = translateVSCodeWhen('resourceLangId == typescript');
		expect(result.ok).toBe(false);
	});

	it('rejects a clause that does not parse', () => {
		const result = translateVSCodeWhen('&& terminalFocus');
		expect(result.ok).toBe(false);
	});
});

describe('buildVSCodeDiff', () => {
	function row(over: Partial<{ key: string; command: string; when: string }>) {
		return buildVSCodeDiff([over])[0];
	}

	it('skips a rule missing key or command', () => {
		expect(row({ command: 'workbench.action.quickOpen' }).kind).toBe('skip');
		expect(row({ key: 'ctrl+p' }).kind).toBe('skip');
	});

	it('skips a negative (unbind) rule', () => {
		expect(row({ key: 'ctrl+p', command: '-workbench.action.quickOpen' }).kind).toBe('skip');
	});

	it('skips a command outside the frozen id-map core, with a reason', () => {
		const r = row({ key: 'ctrl+shift+m', command: 'workbench.action.showAllSymbols' });
		expect(r.kind).toBe('skip');
		expect(r.detail).toMatch(/not in the frozen VS Code command map/);
	});

	it('skips the explicit no-equivalent entry', () => {
		const r = row({ key: 'ctrl+shift+t', command: 'workbench.action.terminal.new' });
		expect(r.kind).toBe('skip');
		expect(r.detail).toMatch(/no Ikenga equivalent/);
	});

	it('adds a mapped command whose key is free', () => {
		const r = row({ key: 'ctrl+alt+shift+f19', command: 'workbench.action.quickOpen' });
		expect(r.kind).toBe('add');
		expect(r.write).toEqual({ key: 'mod+alt+shift+f19', command: 'palette.projects' });
	});

	it('reports a clash when the translated key is already held — even by the same target command', () => {
		// G-ACTIONS §10.5's own worked example: `mod+b` is `explorer.toggle`'s
		// own default key, and this row maps onto `explorer.toggle` too.
		const r = row({ key: 'ctrl+b', command: 'workbench.action.toggleSidebarVisibility' });
		expect(r.kind).toBe('clash');
		expect(r.detail).toMatch(/imports unbound/);
	});
});
