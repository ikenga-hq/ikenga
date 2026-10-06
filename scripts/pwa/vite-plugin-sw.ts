// plans/pwa S1 (W2): emit `dist/sw.js` for the browser-served build.
//
// No vite-plugin-pwa / workbox: the worker is ~100 lines (`pwa/sw.ts`) and
// its rules are unit-tested in `src/lib/pwa/sw-logic.ts`. This plugin only
//   1. computes the precache list from the real bundle (`collectPrecache`),
//   2. hashes it into a BUILD_ID, and
//   3. bundles `pwa/sw.ts` with esbuild (Vite 6's own bundler dependency) as a
//      classic iife with both inlined, emitted as `sw.js` at the dist root.
// Any change to a precached file changes the list, the BUILD_ID and so the
// bytes of `sw.js`, which is what drives the "Reload to update" flow.
//
// The desktop app embeds the same dist but never registers the worker
// (`src/lib/pwa/register.ts` refuses under Tauri), so emitting it is harmless.

import { createHash } from 'node:crypto';
import { readdirSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { build } from 'esbuild';
import type { Plugin } from 'vite';
import { collectPrecache, type PrecacheBundle } from '../../src/lib/pwa/precache';

export interface SwPluginOptions {
	/** Project root (holds `pwa/`, `public/`). */
	root: string;
}

/** Files from `public/` the shell needs to start and to be installable. */
function publicExtras(root: string): string[] {
	const extras = ['index.html', 'manifest.webmanifest'];
	try {
		for (const f of readdirSync(path.join(root, 'public', 'icons'))) {
			if (f.endsWith('.png')) extras.push(`icons/${f}`);
		}
	} catch {
		// No icons directory: the manifest still loads, the install prompt
		// just won't have an icon. Not a reason to fail the build.
	}
	return extras;
}

/**
 * The first 12 hex chars of a sha256 over the precache list, plus the bytes of
 * the files whose names carry no content hash (`index.html` template, the
 * manifest, the icons), so editing any of those still ships a new worker.
 */
export function buildIdFor(list: readonly string[], unhashedFiles: readonly string[]): string {
	const h = createHash('sha256').update(list.join('\n'));
	for (const file of unhashedFiles) {
		h.update('\0');
		try {
			h.update(readFileSync(file));
		} catch {
			// Missing is a state too; the list already records what exists.
		}
	}
	return h.digest('hex').slice(0, 12);
}

export function swPlugin(opts: SwPluginOptions): Plugin {
	return {
		name: 'ikenga-pwa-sw',
		apply: 'build',
		generateBundle: {
			// After Vite's own generateBundle work (pure-CSS chunk removal,
			// `index.html` emission), so the walk sees the bundle as written.
			order: 'post',
			async handler(_options, bundle) {
				const extras = publicExtras(opts.root);
				const precache = collectPrecache(bundle as unknown as PrecacheBundle, {
					bootModuleSuffix: 'src/boot/primary.tsx',
					extra: extras,
				});
				// Every listed build file must really be in the output: one 404
				// makes `cache.addAll` reject and the worker never installs.
				const missing = precache.filter(
					(p) => !extras.includes(p.slice(1)) && !(p.slice(1) in bundle)
				);
				if (missing.length > 0) {
					throw new Error(
						`pwa: precache lists files the build did not emit: ${missing.join(', ')}`
					);
				}
				const unhashed = extras.map((f) =>
					f === 'index.html' ? path.join(opts.root, f) : path.join(opts.root, 'public', f)
				);
				const buildId = buildIdFor(precache, unhashed);

				const result = await build({
					entryPoints: [path.join(opts.root, 'pwa', 'sw.ts')],
					bundle: true,
					write: false,
					format: 'iife',
					platform: 'browser',
					target: 'es2022',
					minify: true,
					legalComments: 'none',
					define: {
						__PRECACHE__: JSON.stringify(precache),
						__BUILD_ID__: JSON.stringify(buildId),
					},
				});
				const out = result.outputFiles[0];
				if (!out) throw new Error('pwa: esbuild produced no sw.js');

				this.emitFile({ type: 'asset', fileName: 'sw.js', source: out.text });
				this.info(`sw.js: build ${buildId}, ${precache.length} precached files`);
			},
		},
	};
}
