// WP-61 unit tests — fix round 1, item 2: every frozen lookup table in
// `vscode-map.ts` is built on a null prototype (`frozenTable`), so a
// bracket lookup by an inherited `Object.prototype` name never resolves to
// anything. Written under DEC-50, not run this session.

import { describe, expect, it } from 'vitest';
import { ICON_GLYPH_ALIASES, resolveImportIcon, VSCODE_COMMAND_MAP, VSCODE_WHEN_KEY_MAP } from './vscode-map';

const POISON_NAMES = ['constructor', '__proto__', 'toString', 'hasOwnProperty'];

describe('frozen import tables never resolve a prototype-chain name', () => {
	it('VSCODE_COMMAND_MAP', () => {
		for (const poison of POISON_NAMES) {
			expect(Object.hasOwn(VSCODE_COMMAND_MAP, poison)).toBe(false);
			expect(VSCODE_COMMAND_MAP[poison]).toBeUndefined();
			expect(poison in VSCODE_COMMAND_MAP).toBe(false);
		}
	});

	it('VSCODE_WHEN_KEY_MAP', () => {
		for (const poison of POISON_NAMES) {
			expect(Object.hasOwn(VSCODE_WHEN_KEY_MAP, poison)).toBe(false);
			expect(VSCODE_WHEN_KEY_MAP[poison]).toBeUndefined();
		}
	});

	it('ICON_GLYPH_ALIASES, through resolveImportIcon', () => {
		for (const poison of POISON_NAMES) {
			expect(Object.hasOwn(ICON_GLYPH_ALIASES, poison)).toBe(false);
			// Passes through unchanged (the icon component's own fallback then
			// warns and renders `zap`) rather than resolving to `[Function:
			// toString]` or similar.
			expect(resolveImportIcon(poison)).toBe(poison);
		}
	});
});
