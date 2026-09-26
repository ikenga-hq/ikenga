// WP-61 unit tests — written under DEC-50, not run this session (WP-63 runs
// them). Covers the JSONC reader, the key/`when` translators and the diff
// classifier against the DEFAULT-only keymap (this file never calls
// `startActionsStore()`, so `keyHolder()` reads the module's initial
// `EMPTY_MODEL` — the default layer only, exactly `DEFAULT_KEYMAP` merged —
// which is enough to exercise "free" vs "held" without a store harness).

import { describe, expect, it } from 'vitest';
import {
	buildVSCodeDiff,
	detectVSCodeSourcePlatform,
	parseVSCodeKeybindingsText,
	translateVSCodeKey,
	translateVSCodeWhen,
} from './vscode';
import { MAX_IMPORT_FILE_BYTES, VSCODE_COMMAND_MAP } from './vscode-map';

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

	it('does not strip a comma immediately followed by `]`/`}` inside a string literal (fix round 1, "low" item)', () => {
		// The old regex-based `stripTrailingCommas` matched `,(\s*[}\]])`
		// against the raw text with no idea it was inside a string — a `when`
		// value whose content happens to contain `,]` or `,}` adjacent would
		// silently lose the comma.
		const rows = parseVSCodeKeybindingsText(`[{"key":"a","command":"b","when":"x =~ 'a,]b'"}]`);
		expect(rows[0].when).toBe("x =~ 'a,]b'");
	});

	it('refuses a file over the size cap before parsing (fix round 1, item 8)', () => {
		const huge = `[${'x'.repeat(MAX_IMPORT_FILE_BYTES + 1)}]`;
		expect(() => parseVSCodeKeybindingsText(huge)).toThrow(/capped at 1 MiB/);
	});
});

describe('detectVSCodeSourcePlatform (fix round 1, item 6)', () => {
	it('detects mac from a `cmd` token anywhere in the file', () => {
		expect(detectVSCodeSourcePlatform([{ key: 'ctrl+t', command: 'a' }, { key: 'cmd+shift+p', command: 'b' }])).toBe('mac');
	});

	it('defaults to `other` with no `cmd`/`command` token', () => {
		expect(detectVSCodeSourcePlatform([{ key: 'ctrl+t', command: 'a' }, { key: 'win+e', command: 'b' }])).toBe('other');
	});
});

describe('translateVSCodeKey', () => {
	it('a mac file: `cmd` folds onto `mod`, `ctrl` stays literal (fix round 1, item 6)', () => {
		expect(translateVSCodeKey('cmd+b', 'mac')).toBe('mod+b');
		expect(translateVSCodeKey('ctrl+t', 'mac')).toBe('ctrl+t');
	});

	it('a Windows/Linux file: `ctrl` folds onto `mod`, `win`/`meta` stay literal, never fold into `mod` (fix round 1, item 6)', () => {
		expect(translateVSCodeKey('ctrl+b', 'other')).toBe('mod+b');
		expect(translateVSCodeKey('win+e', 'other')).toBe('meta+e');
		expect(translateVSCodeKey('meta+e', 'other')).toBe('meta+e');
	});

	it('cmd+shift+p on a mac file folds cmd onto mod', () => {
		expect(translateVSCodeKey('cmd+shift+p', 'mac')).toBe('mod+shift+p');
	});

	it('keeps ctrl+cmd literal when both are held on a mac file', () => {
		expect(translateVSCodeKey('ctrl+cmd+p', 'mac')).toBe('ctrl+meta+p');
	});

	it('keeps ctrl+win literal when both are held on a Windows/Linux file', () => {
		expect(translateVSCodeKey('ctrl+win+t', 'other')).toBe('ctrl+meta+t');
	});

	it('translates a two-stroke chord', () => {
		expect(translateVSCodeKey('ctrl+k ctrl+r', 'other')).toBe('mod+k mod+r');
	});

	it('maps named keys (arrows, escape, …)', () => {
		expect(translateVSCodeKey('up', 'other')).toBe('arrowup');
		expect(translateVSCodeKey('ctrl+escape', 'other')).toBe('mod+escape');
	});

	it('returns null for an unrecognized modifier or key token', () => {
		expect(translateVSCodeKey('ctrl+numpad1', 'other')).toBeNull();
		expect(translateVSCodeKey('hyper+b', 'other')).toBeNull();
	});

	it('returns null for more than two strokes', () => {
		expect(translateVSCodeKey('ctrl+k ctrl+r ctrl+x', 'other')).toBeNull();
	});

	it('never resolves a prototype-chain key or modifier name (fix round 1, item 2)', () => {
		for (const poison of ['constructor', '__proto__', 'toString', 'hasOwnProperty']) {
			expect(translateVSCodeKey(`ctrl+${poison}`, 'other')).toBeNull();
			expect(translateVSCodeKey(`${poison}+t`, 'other')).toBeNull();
		}
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

	it('never rewrites a quoted comparison value, even when it names a mapped context key (fix round 1, "low" item — tokenizer safety)', () => {
		// The substitution used to be a `String.replace` with a `\b…\b` regex
		// over the raw clause text, which had no idea `'editorTextFocus'` on
		// the right of `==` was a string literal rather than the same context
		// key repeated — walking the parsed AST and touching only `key`
		// fields (never `value`/`pattern`) makes that impossible.
		const result = translateVSCodeWhen("editorTextFocus == 'editorTextFocus'");
		expect(result.ok).toBe(true);
		if (result.ok) expect(result.when).toBe("inputFocus == 'editorTextFocus'");
	});

	it('rejects a context key outside the supported subset', () => {
		const result = translateVSCodeWhen('resourceLangId == typescript');
		expect(result.ok).toBe(false);
	});

	it('never treats an inherited `Object.prototype` name as a mapped context key (fix round 1, item 2)', () => {
		// `toString` and `hasOwnProperty` are valid context-key *identifiers*
		// (they parse), so a plain-object lookup table would resolve them to
		// `Object.prototype`'s own methods instead of `undefined`.
		for (const poison of ['toString', 'hasOwnProperty', 'constructor']) {
			const result = translateVSCodeWhen(poison);
			expect(result.ok).toBe(false);
		}
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

	it('reports "already bound", not a clash, when held by the same target command (fix round 1, item 7)', () => {
		// `mod+b` is `explorer.toggle`'s own default key, and this row maps
		// onto `explorer.toggle` too (G-ACTIONS §10.5's worked example) — still
		// unbound either way, but now reported as the harmless "already have
		// it" case, matching `package.ts`'s identical rule.
		const r = row({ key: 'ctrl+b', command: 'workbench.action.toggleSidebarVisibility' });
		expect(r.kind).toBe('skip');
		expect(r.detail).toMatch(/already bound/);
	});

	it('reports a clash when the translated key is held by a *different* command', () => {
		const r = row({ key: 'ctrl+j', command: 'workbench.action.quickOpen' }); // mod+j is companion.toggle's default
		expect(r.kind).toBe('clash');
		expect(r.detail).toMatch(/imports unbound/);
	});

	it('the id-map core no longer maps `showCommands` to `palette.open` (fix round 1, item 11)', () => {
		expect(Object.hasOwn(VSCODE_COMMAND_MAP, 'workbench.action.showCommands')).toBe(false);
		const r = row({ key: 'ctrl+shift+p', command: 'workbench.action.showCommands' });
		expect(r.kind).toBe('skip');
		expect(r.detail).toMatch(/not in the frozen VS Code command map/);
	});

	it('the second of two rows asking for the same free key in one import is a clash (fix round 1, item 4)', () => {
		const rows = buildVSCodeDiff([
			{ key: 'ctrl+alt+shift+f19', command: 'workbench.action.quickOpen' },
			{ key: 'ctrl+alt+shift+f19', command: 'workbench.action.toggleSidebarVisibility' },
		]);
		expect(rows[0].kind).toBe('add');
		expect(rows[1].kind).toBe('clash');
		expect(rows[1].detail).toMatch(/earlier in this import/);
	});

	it('never resolves a prototype-chain name as a command (fix round 1, item 2)', () => {
		for (const poison of ['constructor', '__proto__', 'toString', 'hasOwnProperty']) {
			const r = row({ key: 'ctrl+alt+shift+f20', command: poison });
			expect(r.kind).toBe('skip');
			expect(r.detail).toMatch(/not in the frozen VS Code command map/);
		}
	});
});
