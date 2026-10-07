import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { describe, expect, it } from 'vitest';
import {
	LAZY_MAP_MODULE,
	namesModule,
	parseIconTable,
	staticMapModule,
} from '../../../scripts/vite-plugin-lucide-icons';

const SAMPLE = `const dynamicIconImports = {
  "a-arrow-down": () => import('./icons/a-arrow-down.mjs'),
  "bar-chart": () => import('./icons/chart-no-axes-column.mjs'),
  "chart-no-axes-column": () => import('./icons/chart-no-axes-column.mjs'),
};`;

describe('lucide icons vite plugin', () => {
	it('parses names and files, aliases sharing a file', () => {
		expect(parseIconTable(SAMPLE)).toEqual([
			['a-arrow-down', 'a-arrow-down'],
			['bar-chart', 'chart-no-axes-column'],
			['chart-no-axes-column', 'chart-no-axes-column'],
		]);
	});

	it('the build map has static imports only, one per distinct file, no import()', () => {
		const code = staticMapModule(parseIconTable(SAMPLE));
		expect(code).not.toMatch(/import\(/);
		expect(code.match(/^import /gm)).toHaveLength(2);
		expect(code).toContain('"bar-chart": i1');
		expect(code).toContain('"chart-no-axes-column": i1');
		expect(namesModule(parseIconTable(SAMPLE))).toContain('"bar-chart"');
	});

	it('the dev map stays lazy', () => {
		expect(LAZY_MAP_MODULE).toContain('await table[name]()');
	});

	it('parses every row of the installed lucide-react (no silent drift)', () => {
		const pkg = createRequire(import.meta.url).resolve('lucide-react/package.json');
		const file = path.join(path.dirname(pkg), 'dist/esm/dynamicIconImports.mjs');
		const src = readFileSync(file, 'utf8');
		const rows = src.match(/^\s*"[^"]+":\s*\(\)\s*=>/gm)?.length ?? 0;
		expect(rows).toBeGreaterThan(1000);
		expect(parseIconTable(src)).toHaveLength(rows);
	});
});
