import { existsSync, mkdirSync, rmSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { $ } from 'bun';

const shellRoot = resolve(import.meta.dir, '..');
const pkgDir = join(shellRoot, 'src-tauri', 'resources', 'builtin-pkgs', 'com.ikenga.mcp-iyke');
const srcDir = join(shellRoot, '..', 'ikenga-pkgs', 'packages', 'mcp', 'iyke');

if (!existsSync(pkgDir)) {
	console.error(`error: builtin pkg dir missing: ${pkgDir}`);
	process.exit(1);
}

const distDir = join(pkgDir, 'dist');
mkdirSync(distDir, { recursive: true });

const binDir = join(pkgDir, 'bin');
if (existsSync(binDir)) {
	rmSync(binDir, { recursive: true, force: true });
}

const output = join(distDir, 'index.js');
console.log(`==> bundling iyke-mcp → ${output}`);

const buildResult = await Bun.build({
	entrypoints: [join(srcDir, 'src', 'index.ts')],
	outdir: distDir,
	naming: 'index.js',
	target: 'bun',
	minify: true,
	sourcemap: 'none',
});

if (!buildResult.success) {
	console.error('Build failed:');
	for (const log of buildResult.logs) {
		console.error(log);
	}
	process.exit(1);
}

console.log(`==> done: iyke-mcp bundled to ${output}`);
