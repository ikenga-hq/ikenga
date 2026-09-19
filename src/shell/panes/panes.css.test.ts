// WP-07 P4: reduced-motion path has no transition.
//
// jsdom doesn't run a CSS engine (no PostCSS/Tailwind pipeline, no media-query
// evaluation), so this can't assert computed style the way a browser-mode
// test can (see e2e/panes.spec.ts for that half). What a plain test *can*
// prove — and regress-guard — is that the stylesheet itself carries the
// override: a `prefers-reduced-motion: reduce` block that zeroes the
// `.pane-tools` transition. If someone deletes this rule, this test catches
// it even though nothing renders here.

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

const cssPath = join(dirname(fileURLToPath(import.meta.url)), 'panes.css');
const css = readFileSync(cssPath, 'utf8');

describe('panes.css — reduced motion (§1.4 / P4)', () => {
	it('has a prefers-reduced-motion block', () => {
		expect(css).toMatch(/@media\s*\(prefers-reduced-motion:\s*reduce\)/);
	});

	it('zeroes .pane-tools transition inside that block', () => {
		const match = css.match(/@media\s*\(prefers-reduced-motion:\s*reduce\)\s*\{([\s\S]*?)\n\}/);
		expect(match).not.toBeNull();
		const body = match![1];
		expect(body).toMatch(/\.pane-tools\s*\{[^}]*transition:\s*none/);
	});

	it('carries no color literal (shell-design-system: tokens only)', () => {
		expect(css).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
		expect(css).not.toMatch(/\brgba?\(/);
		expect(css).not.toMatch(/\bhsla?\(/);
	});
});
