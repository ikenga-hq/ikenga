// Phase 5 — Background pkg install queue triggered after the Connectors step.
//
// On connectors-step `goNext`, we kick off installs for every pkg the user
// selected in step 4 that isn't already installed. Each install is a
// fire-and-forget call into the pkg kernel; failures are recorded in
// `onboarding.payload.installResults` so the Summary step can surface
// "X installed, Y failed (retry from /install)".
//
// Pkgs whose connectors the user skipped install but stay `disabled` —
// the kernel handles the disabled flag at install time when it sees the
// declared capability/vault keys aren't resolvable yet. (Phase 6 wires
// `pkgKernelInstall` to take an `enabled` arg; for now we install with
// the kernel default and let the consumer code degrade gracefully.)

import { type PkgKernelStatus, pkgInstallFromPath, pkgKernelStatus } from '@/lib/tauri-cmd';
import { honestRpcError } from '@/lib/transport/unavailable';

import { findCatalogEntry, ONBOARDING_PKG_CATALOG } from './pkg-catalog';

export interface PkgInstallResult {
	pkgId: string;
	display: string;
	ok: boolean;
	skipped: boolean;
	error?: string;
}

export interface InstallQueueInput {
	selectedPkgIds: readonly string[];
	skippedConnectorPkgIds?: ReadonlySet<string>;
	/** Catalog override for tests. */
	catalogResolver?: (id: string) => { installPath: string } | null;
	/** Kernel status override for tests. */
	kernelStatus?: () => Promise<PkgKernelStatus>;
	/** Install function override for tests. */
	install?: (installPath: string) => Promise<unknown>;
}

/**
 * Walk the user's selection and dispatch one `pkg_install_from_path` per
 * pkg that has a local install path AND isn't already installed. Returns a
 * per-pkg result list. This function is intentionally serial — pkg kernel
 * install is fast (~200ms) and serial ordering keeps log output deterministic.
 */
export async function triggerPkgInstalls(input: InstallQueueInput): Promise<PkgInstallResult[]> {
	const {
		selectedPkgIds,
		skippedConnectorPkgIds = new Set<string>(),
		catalogResolver = defaultCatalogResolver,
		kernelStatus = pkgKernelStatus,
		install = (installPath: string) => pkgInstallFromPath(installPath),
	} = input;

	let alreadyInstalled = new Set<string>();
	try {
		const status = await kernelStatus();
		alreadyInstalled = new Set(status.installed.map((p) => p.id));
	} catch {
		// Kernel diag failed — proceed and let install attempts surface the
		// per-pkg error. Better than swallowing the entire batch.
	}

	const results: PkgInstallResult[] = [];

	for (const pkgId of selectedPkgIds) {
		const entry = findCatalogEntry(pkgId);
		const display = entry?.display ?? pkgId;
		if (alreadyInstalled.has(pkgId)) {
			results.push({ pkgId, display, ok: true, skipped: true });
			continue;
		}
		const resolved = catalogResolver(pkgId);
		if (!resolved) {
			// No local path means we can't install from this surface — that's
			// fine, the user can install from /install later. Mark as skipped
			// so the summary tally is accurate.
			results.push({
				pkgId,
				display,
				ok: false,
				skipped: true,
				error: 'No install source resolved.',
			});
			continue;
		}
		try {
			await install(resolved.installPath);
			results.push({ pkgId, display, ok: true, skipped: false });
		} catch (err) {
			results.push({
				pkgId,
				display,
				ok: false,
				skipped: false,
				error: honestRpcError(err),
			});
		}
	}

	// Surface skipped-connector pkgs so the caller can log "pkg X installed
	// but is disabled until you configure Y".
	if (skippedConnectorPkgIds.size > 0) {
		console.info(
			'[onboarding/install] pkgs whose connectors were skipped:',
			Array.from(skippedConnectorPkgIds)
		);
	}

	return results;
}

/**
 * Best-effort resolver for catalog pkg id → local install path. Reads
 * `/install-catalog.json` (the same file the install route uses) at
 * runtime so we don't have to ship a parallel registry. Returns null
 * when no local source is declared for that id.
 */
async function loadCatalogManifest(): Promise<Record<string, string>> {
	try {
		const res = await fetch('/install-catalog.json');
		if (!res.ok) return {};
		const json = (await res.json()) as {
			packages?: Array<{
				id: string;
				source?: { kind?: string; path?: string };
			}>;
		};
		const out: Record<string, string> = {};
		for (const entry of json.packages ?? []) {
			if (entry.source?.kind === 'local' && entry.source.path) {
				out[entry.id] = entry.source.path;
			}
		}
		return out;
	} catch {
		return {};
	}
}

let catalogCache: Record<string, string> | null = null;

function defaultCatalogResolver(id: string): { installPath: string } | null {
	// First time we resolve, fetch the catalog and remember it for the rest
	// of the wizard session. Synchronous-by-design so the queue doesn't have
	// to thread a promise through every call — the resolver returns null
	// for the first pkg if we haven't fetched yet; the caller retries via
	// `prewarmCatalog`.
	const cache = catalogCache;
	if (!cache) return null;
	const path = cache[id];
	return path ? { installPath: path } : null;
}

/**
 * Pre-fetch the install catalog so `defaultCatalogResolver` can answer
 * synchronously. Call before `triggerPkgInstalls` for the default
 * resolver to work.
 */
export async function prewarmCatalog(): Promise<void> {
	catalogCache = await loadCatalogManifest();
}

/** Reset the cache — exposed for tests. */
export function _resetCatalogCacheForTests(): void {
	catalogCache = null;
}

/**
 * Summary of an install batch — used by the Summary step's tally.
 */
export interface InstallBatchSummary {
	total: number;
	installed: number;
	alreadyPresent: number;
	failed: number;
	noSource: number;
}

export function summariseBatch(results: readonly PkgInstallResult[]): InstallBatchSummary {
	let installed = 0;
	let alreadyPresent = 0;
	let failed = 0;
	let noSource = 0;
	for (const r of results) {
		if (r.skipped && r.ok) alreadyPresent++;
		else if (r.skipped && !r.ok) noSource++;
		else if (r.ok) installed++;
		else failed++;
	}
	return {
		total: results.length,
		installed,
		alreadyPresent,
		failed,
		noSource,
	};
}

// Re-export the catalog so the install queue can be driven by a frozen
// snapshot when callers want to ignore catalog evolution mid-run.
export { ONBOARDING_PKG_CATALOG };
