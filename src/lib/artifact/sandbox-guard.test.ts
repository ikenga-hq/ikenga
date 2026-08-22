import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';

import { describe, expect, it } from 'vitest';

// WP-11 regression armour.
//
// `allow-same-origin` on a frame rendering content we did not write is the
// whole vulnerability: it gives the child a real handle on `window.parent`, and
// from there `window.parent.__TAURI_INTERNALS__.invoke` reaches every command in
// `tauri::generate_handler![]`. The flag is also exactly what a well-meaning
// change reaches for when parent→child DOM access stops working ("Studio needs
// it back"), which is roughly how it arrived the first time.
//
// So this is a source-level check rather than a runtime one: it fails the build
// the moment the attribute reappears anywhere outside the allowlist below,
// without needing a shell to be running.

const SRC = join(import.meta.dirname, '../..');

/**
 * Files permitted to set `allow-same-origin`, each with the reason it is safe.
 *
 * Adding an entry here is a security decision. The test names the file so the
 * diff that adds one is obvious in review; it should not be extended to make a
 * failure go away.
 */
const ALLOWED = new Map<string, string>([
	[
		'components/pkg/pkg-iframe-host.tsx',
		// First-party: the document is srcdoc built by the kernel from an installed
		// pkg's own manifest-declared content, not arbitrary generated HTML, and
		// AppBridge relies on same-origin. Artifact frames are the untrusted case
		// and are handled in src/viewer/renderers/html-frame.tsx.
		'first-party pkg content mounted via srcdoc; AppBridge depends on same-origin',
	],
]);

/** `sandbox="… allow-same-origin …"` as an actual attribute, not prose. */
const SANDBOX_ATTR = /sandbox\s*=\s*["'{][^"'}]*allow-same-origin/;

/**
 * Strip comments before matching.
 *
 * Comments quote the attribute legitimately — the surrounding code explains why
 * the flag is or is not set, and that explanation necessarily contains the
 * string. Failing on prose would push authors to reword the explanation rather
 * than fix anything, and would conflate stale documentation with an actual
 * grant. Those are different problems with different fixes.
 */
function stripComments(src: string): string {
	return src.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:])\/\/.*$/gm, '$1');
}

function sourceFiles(dir: string): string[] {
	const out: string[] = [];
	for (const entry of readdirSync(dir)) {
		if (entry === 'node_modules' || entry === 'dist') continue;
		const full = join(dir, entry);
		if (statSync(full).isDirectory()) {
			out.push(...sourceFiles(full));
		} else if (/\.(ts|tsx)$/.test(entry) && !/\.test\.tsx?$/.test(entry)) {
			out.push(full);
		}
	}
	return out;
}

describe('artifact sandbox containment', () => {
	it('no source outside the allowlist grants allow-same-origin', () => {
		const offenders = sourceFiles(SRC)
			.filter((f) => SANDBOX_ATTR.test(stripComments(readFileSync(f, 'utf8'))))
			.map((f) => relative(SRC, f).split('\\').join('/'))
			.filter((rel) => !ALLOWED.has(rel));

		expect(
			offenders,
			`allow-same-origin found in ${offenders.join(', ')}. On a frame rendering ` +
				`content we did not author this re-opens the parent-realm escape to ` +
				`window.parent.__TAURI_INTERNALS__.invoke. If parent->child DOM access ` +
				`is what you need, route it through the postMessage bridge in ` +
				`src/lib/artifact/bridge-messages.ts instead of restoring this flag.`,
		).toEqual([]);
	}, 30000);

	it('the artifact renderer itself never grants it', () => {
		// Called out separately from the sweep above: this is the file the
		// vulnerability lived in, so it gets an assertion that names it rather
		// than relying on it being absent from a list.
		const src = stripComments(readFileSync(join(SRC, 'viewer/renderers/html-frame.tsx'), 'utf8'));
		expect(SANDBOX_ATTR.test(src)).toBe(false);
		// And it must still be sandboxed at all — an empty/absent sandbox attribute
		// would be strictly worse than the flag we are guarding against.
		expect(src).toMatch(/sandbox="allow-scripts"/);
	});

	it('every allowlist entry still exists and still needs the exemption', () => {
		// Keeps the allowlist honest: a stale entry is a standing permission for a
		// file that may no longer set the flag, or may no longer exist.
		for (const [rel] of ALLOWED) {
			const body = stripComments(readFileSync(join(SRC, rel), 'utf8'));
			expect(SANDBOX_ATTR.test(body), `${rel} is allowlisted but no longer sets allow-same-origin — remove the entry`).toBe(
				true,
			);
		}
	});
});
