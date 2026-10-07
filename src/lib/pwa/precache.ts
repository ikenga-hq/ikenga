// plans/pwa S1 (W2): which built files the service worker precaches.
//
// Pure, so vitest can drive it with a fake Rollup bundle; the build-time
// plugin (`scripts/pwa/vite-plugin-sw.ts`) hands it the real one.
//
// The precache must be enough to BOOT offline, not just to load `main.tsx`:
// `main.tsx` reaches the workspace through a dynamic `import('@/boot/primary')`
// (multi-window WP-05), so the entry chunk's static closure alone is two files
// and boots nothing. The list is therefore the entry's static closure plus the
// static closure of the boot chunk. Lazy routes stay out (they are cached at
// runtime the first time they load), as do `@tauri-apps/*` chunks, which a
// browser never runs.

/** The parts of a Rollup `OutputChunk` this walk reads. */
export interface PrecacheChunk {
	type: 'chunk';
	fileName: string;
	isEntry: boolean;
	imports: string[];
	dynamicImports: string[];
	facadeModuleId: string | null;
	moduleIds: string[];
	viteMetadata?: { importedCss: Set<string>; importedAssets: Set<string> };
}

/** The parts of a Rollup `OutputAsset` this walk reads. */
export interface PrecacheAsset {
	type: 'asset';
	fileName: string;
}

export type PrecacheBundle = Record<string, PrecacheChunk | PrecacheAsset>;

export interface PrecacheOptions {
	/** Module path suffix of the dynamically imported boot chunk. */
	bootModuleSuffix: string;
	/** Files outside the bundle to include as-is (`index.html`, icons, …),
	 *  relative to the dist root. */
	extra: readonly string[];
}

const TAURI_MODULE =
	/[\\/]node_modules[\\/](?:\.pnpm[\\/][^\\/]+[\\/]node_modules[\\/])?@tauri-apps[\\/]/;

/**
 * A JS chunk that only imports CSS. Vite deletes these from the bundle (their
 * CSS is attached to the importer) — often in a `generateBundle` that runs
 * after ours — so listing one would make `cache.addAll` 404 and the worker
 * would never install.
 */
export function isPureCssChunk(chunk: PrecacheChunk): boolean {
	return chunk.moduleIds.length > 0 && chunk.moduleIds.every((id) => /\.css(\?|$)/.test(id));
}

/** A chunk built only from `@tauri-apps/*` modules. */
export function isTauriOnlyChunk(chunk: PrecacheChunk): boolean {
	return chunk.moduleIds.length > 0 && chunk.moduleIds.every((id) => TAURI_MODULE.test(id));
}

function normalize(id: string): string {
	return id.replace(/\\/g, '/');
}

/**
 * The URL paths (leading `/`, sorted, de-duplicated) the service worker
 * precaches. Throws when the bundle has no entry or no boot chunk: a worker
 * that precaches a shell that cannot boot is worse than no worker.
 */
export function collectPrecache(bundle: PrecacheBundle, opts: PrecacheOptions): string[] {
	const chunks = new Map<string, PrecacheChunk>();
	for (const item of Object.values(bundle)) {
		if (item.type === 'chunk') chunks.set(item.fileName, item);
	}

	const entries = [...chunks.values()].filter((c) => c.isEntry);
	if (entries.length === 0) throw new Error('pwa precache: the bundle has no entry chunk');

	const suffix = normalize(opts.bootModuleSuffix);
	const boot = [...chunks.values()].find(
		(c) => c.facadeModuleId !== null && normalize(c.facadeModuleId).endsWith(suffix)
	);
	if (!boot) {
		throw new Error(`pwa precache: no chunk is built from ${opts.bootModuleSuffix}`);
	}

	const files = new Set<string>();
	const seen = new Set<string>();
	const walk = (fileName: string) => {
		if (seen.has(fileName)) return;
		seen.add(fileName);
		const chunk = chunks.get(fileName);
		if (!chunk) return;
		if (isTauriOnlyChunk(chunk)) return;
		if (!isPureCssChunk(chunk)) files.add(chunk.fileName);
		for (const css of chunk.viteMetadata?.importedCss ?? []) files.add(css);
		for (const asset of chunk.viteMetadata?.importedAssets ?? []) files.add(asset);
		for (const dep of chunk.imports) walk(dep);
	};
	for (const entry of entries) walk(entry.fileName);
	walk(boot.fileName);

	for (const extra of opts.extra) files.add(extra.replace(/^\/+/, ''));

	return [...files].map((f) => `/${f}`).sort();
}
