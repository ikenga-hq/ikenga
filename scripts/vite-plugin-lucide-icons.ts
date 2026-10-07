// Boot perf: every name-resolved lucide icon in ONE lazily-loaded chunk.
//
// `lucide-react/dynamic` resolves a name through ~1.9k per-icon `import()`s,
// so a production build emitted one tiny JS file per icon and a browser that
// touched a few dozen of them paid hundreds of requests on boot. This plugin
// serves two virtual modules built from lucide's own `dynamicIconImports.mjs`
// (so the set can never drift from the installed lucide-react):
//
//   virtual:lucide-icon-names  the kebab-case name list. Small, static, sync;
//                              answers "is this a real icon name?" at boot.
//   virtual:lucide-icon-map    `resolveIcon(name)`. The only importer is the
//                              loader's single `import()` (src/lib/icons), so in
//                              a build every icon module lands in one chunk.
//
// In `vite dev` / vitest the map is lucide's own lazy per-icon table instead:
// the dev server has no chunk-count problem, and a static map of 1.9k deep
// imports would make the dep optimizer re-bundle and reload the page.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import type { Plugin } from 'vite';

export const NAMES_ID = 'virtual:lucide-icon-names';
export const MAP_ID = 'virtual:lucide-icon-map';

/** `"a-arrow-down": () => import('./icons/a-arrow-down.mjs'),` rows. */
const ROW = /^\s*"([^"]+)":\s*\(\)\s*=>\s*import\('\.\/icons\/([^']+)\.mjs'\)/gm;

/** kebab name → icon file stem (aliases share a file with their canonical name). */
export function parseIconTable(source: string): Array<[name: string, file: string]> {
	return [...source.matchAll(ROW)].map((m) => [m[1], m[2]]);
}

export function namesModule(table: ReadonlyArray<[string, string]>): string {
	return `export default ${JSON.stringify(table.map(([name]) => name))};\n`;
}

/** Build flavour: static imports, so Rollup puts every icon in this module's chunk. */
export function staticMapModule(table: ReadonlyArray<[string, string]>): string {
	const files = [...new Set(table.map(([, file]) => file))];
	const ident = new Map(files.map((f, i) => [f, `i${i}`]));
	const imports = files
		.map((f) => `import ${ident.get(f)} from 'lucide-react/dist/esm/icons/${f}.mjs';`)
		.join('\n');
	const rows = table.map(([name, file]) => `\t${JSON.stringify(name)}: ${ident.get(file)},`);
	return `${imports}\nconst map = {\n${rows.join('\n')}\n};\nexport function resolveIcon(name) {\n\treturn Object.hasOwn(map, name) ? map[name] : undefined;\n}\n`;
}

/** Dev / test flavour: lucide's own lazy table, same `resolveIcon` shape. */
export const LAZY_MAP_MODULE = `import table from 'lucide-react/dynamicIconImports';
export async function resolveIcon(name) {
	return Object.hasOwn(table, name) ? (await table[name]()).default : undefined;
}
`;

export function lucideIconsPlugin(): Plugin {
	let isBuild = false;
	let table: Array<[string, string]> | null = null;
	const readTable = () => {
		if (table) return table;
		const pkgDir = path.dirname(
			createRequire(import.meta.url).resolve('lucide-react/package.json')
		);
		const src = readFileSync(path.join(pkgDir, 'dist/esm/dynamicIconImports.mjs'), 'utf8');
		table = parseIconTable(src);
		if (table.length < 100) {
			throw new Error(
				`lucide-icons: parsed only ${table.length} icons from dynamicIconImports.mjs; has lucide-react changed its layout?`
			);
		}
		return table;
	};
	return {
		name: 'ikenga-lucide-icons',
		enforce: 'pre',
		configResolved(config) {
			isBuild = config.command === 'build';
		},
		resolveId(id) {
			return id === NAMES_ID || id === MAP_ID ? `\0${id}` : null;
		},
		load(id) {
			if (id === `\0${NAMES_ID}`) return namesModule(readTable());
			if (id === `\0${MAP_ID}`) return isBuild ? staticMapModule(readTable()) : LAZY_MAP_MODULE;
			return null;
		},
	};
}
