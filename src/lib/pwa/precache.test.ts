// plans/pwa S1 (W2): the precache must be enough to boot offline — the entry's
// static closure PLUS the dynamically imported boot chunk's — and nothing lazy
// or desktop-only.

import { describe, expect, it } from 'vitest';
import {
	collectPrecache,
	isTauriOnlyChunk,
	type PrecacheBundle,
	type PrecacheChunk,
} from './precache';

function chunk(fileName: string, over: Partial<PrecacheChunk> = {}): PrecacheChunk {
	return {
		type: 'chunk',
		fileName,
		isEntry: false,
		imports: [],
		dynamicImports: [],
		facadeModuleId: null,
		moduleIds: [`/repo/src/${fileName}.ts`],
		viteMetadata: { importedCss: new Set(), importedAssets: new Set() },
		...over,
	};
}

function bundleOf(...items: PrecacheChunk[]): PrecacheBundle {
	const b: PrecacheBundle = {
		'assets/index-e1.css': { type: 'asset', fileName: 'assets/index-e1.css' },
	};
	for (const c of items) b[c.fileName] = c;
	return b;
}

const OPTS = {
	bootModuleSuffix: 'src/boot/primary.tsx',
	extra: ['index.html', 'icons/icon-192.png'],
};

function realisticBundle(): PrecacheBundle {
	return bundleOf(
		chunk('assets/index-e1.js', {
			isEntry: true,
			imports: ['assets/vendor-v1.js'],
			dynamicImports: ['assets/primary-p1.js', 'assets/detached-d1.js'],
			facadeModuleId: '/repo/index.html',
			viteMetadata: { importedCss: new Set(['assets/index-e1.css']), importedAssets: new Set() },
		}),
		chunk('assets/vendor-v1.js'),
		chunk('assets/primary-p1.js', {
			facadeModuleId: '/repo/src/boot/primary.tsx',
			imports: ['assets/router-r1.js', 'assets/vendor-v1.js'],
			dynamicImports: ['assets/route-settings-s1.js', 'assets/tauri-core-t1.js'],
			viteMetadata: {
				importedCss: new Set(['assets/primary-p1.css']),
				importedAssets: new Set(['assets/font-f1.woff2']),
			},
		}),
		chunk('assets/router-r1.js', { imports: ['assets/vendor-v1.js'] }),
		chunk('assets/detached-d1.js', { facadeModuleId: '/repo/src/boot/detached.tsx' }),
		chunk('assets/route-settings-s1.js', { facadeModuleId: '/repo/src/routes/settings.tsx' }),
		chunk('assets/tauri-core-t1.js', {
			moduleIds: ['/repo/node_modules/@tauri-apps/api/core.js'],
		})
	);
}

describe('collectPrecache', () => {
	it('takes the entry closure and the boot chunk closure, with their CSS and assets', () => {
		expect(collectPrecache(realisticBundle(), OPTS)).toEqual([
			'/assets/font-f1.woff2',
			'/assets/index-e1.css',
			'/assets/index-e1.js',
			'/assets/primary-p1.css',
			'/assets/primary-p1.js',
			'/assets/router-r1.js',
			'/assets/vendor-v1.js',
			'/icons/icon-192.png',
			'/index.html',
		]);
	});

	it('leaves out lazy routes, the detached-window boot and Tauri-only chunks', () => {
		const list = collectPrecache(realisticBundle(), OPTS);
		expect(list).not.toContain('/assets/route-settings-s1.js');
		expect(list).not.toContain('/assets/detached-d1.js');
		expect(list).not.toContain('/assets/tauri-core-t1.js');
	});

	it('drops a Tauri-only chunk even when something imports it statically', () => {
		const b = realisticBundle();
		(b['assets/router-r1.js'] as PrecacheChunk).imports.push('assets/tauri-core-t1.js');
		expect(collectPrecache(b, OPTS)).not.toContain('/assets/tauri-core-t1.js');
	});

	it('lists the CSS of a pure-CSS chunk but never the chunk Vite deletes', () => {
		const b = realisticBundle();
		(b['assets/primary-p1.js'] as PrecacheChunk).imports.push('assets/xterm-x1.js');
		b['assets/xterm-x1.js'] = chunk('assets/xterm-x1.js', {
			moduleIds: ['/repo/node_modules/@xterm/xterm/css/xterm.css'],
			viteMetadata: { importedCss: new Set(['assets/xterm-x1.css']), importedAssets: new Set() },
		});
		const list = collectPrecache(b, OPTS);
		expect(list).toContain('/assets/xterm-x1.css');
		expect(list).not.toContain('/assets/xterm-x1.js');
	});

	it('is sorted and de-duplicated, so the BUILD_ID is stable for the same build', () => {
		const a = collectPrecache(realisticBundle(), OPTS);
		const b = collectPrecache(realisticBundle(), {
			...OPTS,
			extra: [...OPTS.extra, '/index.html'],
		});
		expect(b).toEqual(a);
		expect([...a].sort()).toEqual(a);
		expect(new Set(a).size).toBe(a.length);
	});

	it('refuses a bundle it cannot boot from', () => {
		const noBoot = bundleOf(chunk('assets/index-e1.js', { isEntry: true }));
		expect(() => collectPrecache(noBoot, OPTS)).toThrow(/primary\.tsx/);
		const noEntry = bundleOf(
			chunk('assets/primary-p1.js', { facadeModuleId: '/r/src/boot/primary.tsx' })
		);
		expect(() => collectPrecache(noEntry, OPTS)).toThrow(/no entry/);
	});

	it('recognises Windows module paths for the boot chunk and Tauri modules', () => {
		const b = bundleOf(
			chunk('assets/index-e1.js', { isEntry: true }),
			chunk('assets/primary-p1.js', { facadeModuleId: 'C:\\repo\\src\\boot\\primary.tsx' })
		);
		expect(collectPrecache(b, OPTS)).toContain('/assets/primary-p1.js');
		expect(
			isTauriOnlyChunk(
				chunk('x', { moduleIds: ['C:\\repo\\node_modules\\@tauri-apps\\api\\core.js'] })
			)
		).toBe(true);
		expect(
			isTauriOnlyChunk(
				chunk('x', {
					moduleIds: ['/repo/node_modules/@tauri-apps/api/core.js', '/repo/src/lib/x.ts'],
				})
			)
		).toBe(false);
	});
});
