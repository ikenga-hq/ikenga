// Shell-side wrapper around @ikenga/registry-client. Bakes in the two
// constants that turn a generic library into "the Ikenga registry":
//   - REGISTRY_URL   — where to fetch index.json
//   - REGISTRY_PUBKEY — the minisign public key the index must verify against
//
// Both ship as build-time constants in the shell binary. They are NOT
// configurable at runtime by design: a future "alternative registry"
// feature must explicitly add a new code path, not silently retarget the
// existing one.

import {
	fetchIndex as fetchIndexLib,
	fetchPkgDetail as fetchPkgDetailLib,
	resolveInstallPlan as resolveInstallPlanLib,
	type FetchedIndex,
	type InstallStep,
	type PkgDetail,
	type RegistryEntry,
} from '@ikenga/registry-client';
import {
	PkgVersionSchema,
	pkgDetailPath,
	type PkgVersion as StorePkgVersion,
} from '@ikenga/contract/registry';

/** Live registry. Source: docs/plans/2026-05-13-ikenga-pkgs-migration.md Phase C. */
export const REGISTRY_URL = 'https://registry.ikenga.dev/index.json';

/**
 * Primitive catalog (Ọba WP-10b). A separate signed `primitives.json` published
 * to the same registry host, signed with the same key as `index.json`. Fetched
 * + minisign-verified by `lib/registry/primitives.ts::fetchPrimitiveCatalog`.
 */
export const PRIMITIVES_URL = 'https://registry.ikenga.dev/primitives.json';

/**
 * Minisign public key for the registry signer. Generated 2026-05-13 by the
 * `update-registry-index.mjs` keypair (NOT the shell updater key — separate
 * trust roots). Hard-coded so the verifier doesn't depend on disk state.
 */
export const REGISTRY_PUBKEY = 'RWRTqugAYXnZRgZPMyuqRNB3G41wg+AhSU2yT8nmDNNQlWQPeCfRXAvI';

export type {
	FetchedIndex,
	InstallStep,
	PkgDetail,
	RegistryEntry,
	RegistryIndex,
	PkgVersion,
} from '@ikenga/registry-client';
export type { PkgVersion as StorePkgVersion } from '@ikenga/contract/registry';

/** Fetch + verify the registry index. Throws on any failure (see lib docs). */
export async function fetchIndex(signal?: AbortSignal): Promise<FetchedIndex> {
	return fetchIndexLib({
		indexUrl: REGISTRY_URL,
		publicKey: REGISTRY_PUBKEY,
		signal,
	});
}

/** Lazy detail fetch — used when the user opens the per-pkg pane. */
export async function fetchPkgDetail(
	indexUrl: string,
	entry: RegistryEntry | { name: string },
	signal?: AbortSignal
): Promise<PkgDetail> {
	return fetchPkgDetailLib({ indexUrl, entry, signal });
}

/**
 * Resolve a full install plan for `root` at `version` (or latest).
 * `getDetail` is supplied by the caller so the UI can dedupe detail-file
 * fetches across multiple install flows.
 */
export async function resolveInstallPlan(
	root: PkgDetail,
	getDetail: (name: string) => Promise<PkgDetail>,
	version?: string
): Promise<InstallStep[]> {
	return resolveInstallPlanLib({ root, version, fetchDetail: getDetail });
}

/**
 * Detail fetch for the Ngwa Store install sheet: the one version the sheet
 * shows, parsed with the shell's own (workspace) `@ikenga/contract` schema
 * rather than the one pinned inside `@ikenga/registry-client`. The pinned
 * schema predates `requires[]` and `signature`, and Zod strips unknown keys,
 * so `fetchPkgDetail` above silently drops the closure and signature the
 * sheet has to show before the user consents. Only the wanted version is
 * validated — older versions in the same file may use retired manifest
 * fields (e.g. `ui.nav`) that the current schema rejects outright.
 * Same URL resolution as the library.
 */
export async function fetchPkgVersionForStore(
	indexUrl: string,
	entry: RegistryEntry | { name: string },
	version: string,
	signal?: AbortSignal
): Promise<StorePkgVersion> {
	const relPath = 'detail' in entry && entry.detail ? entry.detail : pkgDetailPath(entry.name);
	const url = new URL(relPath, indexUrl).toString();
	const res = await fetch(url, { signal });
	if (!res.ok) {
		throw new Error(`Registry detail fetch failed: ${res.status} ${res.statusText} (${url})`);
	}
	const json = (await res.json()) as { versions?: Array<{ version?: unknown }> };
	const versions = Array.isArray(json?.versions) ? json.versions : [];
	const raw = versions.find((v) => v?.version === version) ?? versions[0];
	if (!raw) throw new Error(`Registry detail for ${entry.name} lists no versions`);
	return PkgVersionSchema.parse(raw);
}
