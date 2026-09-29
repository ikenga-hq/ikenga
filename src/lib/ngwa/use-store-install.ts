// Install / update for the Ngwa Store (WP-15 / locked D-02).
//
// Walks the same signed-registry path as the v2 install sheet and the batch
// updater (`lib/registry/install-plan.ts`): fetch the pkg's detail file,
// resolve its dep plan, one `pkgInstallFromRegistry` per step. An update is
// the same call — the kernel treats a registry install over an existing pkg
// dir as an in-place upgrade.
//
// Scope mapping (Store → `PkgScopeWire`), following the Scopes surface's
// `wireOf` convention:
//   - 'personal' → 'workspace'. The pkg kernel has no "personal" scope; a pkg
//     installed with no project (`project_id = None`, wire "workspace") is
//     always loaded, and the Ngwa snapshot reports it as scope `personal`.
//   - 'project'  → 'project:<active project id>', or null (the kernel's
//     default: the active project) when the shell doesn't know the id yet.
//   - update     → the installed item's own scope, so updating never moves a
//     pkg between scopes.
//
// R57 adds the Ọba (vault) path beside the registry one, for git / npx
// primitives — a signed-catalog row or a source resolved from a URL:
//   fetch   → `obaInstallWithDeps` / `obaInstallGit` / `obaInstallNpx`, always
//             with the reviewed pin (`{sha, hash}`), so what is installed is
//             what the sheet showed (N-C; a mismatch rejects, nothing written);
//   place   → `claudePrimitiveEnable` for each dep the fetch materialized
//             (enable order) and the target, in the chosen scope. Install
//             never places, so placing is the last stage of *installing*;
//   Q5      → a dep already in the vault but not reachable from that scope
//             (not enabled there, nor in `workspace`, which every project
//             loads) is enabled there too.
// Primitive scopes are Ọba's: personal = 'workspace', project = `project:<id>`.

import { useQueryClient } from '@tanstack/react-query';
import type { NgwaScope } from '@ikenga/contract';
import type { NgwaCatalogRow, NgwaStoreEntry } from '@/lib/ngwa/enrichment';
import { ngwaSnapshotQueryKey } from '@/lib/ngwa/use-ngwa-snapshot';
import {
	cachedDetailGetter,
	previewIncomingTrust,
	resolveAndInstall,
} from '@/lib/registry/install-plan';
import {
	catalogGitRef,
	catalogPin,
	catalogRefs,
	type PrimitiveCatalogEntry,
} from '@/lib/registry/primitives';
import { registryKeys, useRegistryIndex } from '@/lib/registry/use-registry';
import { queryKeys } from '@/lib/query-keys';
import { useShellStore } from '@/lib/shell/shell-store';
import {
	claudePrimitiveEnable,
	obaInstallGit,
	obaInstallNpx,
	obaInstallWithDeps,
	obaResolveSource,
	obaUpdate,
	type ClaudeStoreEntry,
	type ClaudeStoreKind,
	type ClaudeStoreScope,
	type ObaPin,
	type PkgScopeWire,
	type PrimitiveRef,
	type ResolvedSource,
} from '@/lib/tauri-cmd';

export type StoreInstallScope = 'personal' | 'project';

export function storeScopeWire(
	scope: StoreInstallScope,
	activeProjectId: string | null | undefined
): PkgScopeWire | null {
	if (scope === 'personal') return 'workspace';
	return activeProjectId ? `project:${activeProjectId}` : null;
}

/** An installed item's current scope, as the wire value that keeps it there. */
export function installedScopeWire(scope: NgwaScope | undefined): PkgScopeWire | null {
	if (!scope) return null;
	return scope.kind === 'personal' ? 'workspace' : `project:${scope.project_id}`;
}

/** Ọba's scope for a Store scope choice (personal = the workspace, which
 *  every project loads). Throws when there is no active project to install to. */
export function primitiveScopeWire(
	scope: StoreInstallScope,
	activeProjectId: string | null | undefined
): ClaudeStoreScope {
	if (scope === 'personal') return 'workspace';
	if (!activeProjectId) throw new Error('No active project to install into — pick personal');
	return `project:${activeProjectId}`;
}

/** Where a primitive install is in its run (the sheet's staged button). */
export type PrimitiveInstallStage = 'fetch' | 'place';

/** What a primitive install did, for the sheet's done state. */
export interface PrimitiveInstallOutcome {
	scope: ClaudeStoreScope;
	/** Fetched into the vault and placed in `scope`: the target first, then the
	 *  deps the fetch materialized. */
	placed: PrimitiveRef[];
	/** Q5: already in the vault, not reachable from `scope`, so enabled there. */
	alsoEnabled: PrimitiveRef[];
	/** Already satisfied and reachable (or not a vault item): left where it is. */
	leftInPlace: PrimitiveRef[];
}

/** Inputs every primitive install needs besides its target. */
export interface PrimitiveInstallContext {
	/** The signed catalog (deps resolve against it, at their pins). */
	catalog: readonly PrimitiveCatalogEntry[];
	/** The vault as listed now (Q5 reads each dep's `enabledIn`). */
	vault: readonly ClaudeStoreEntry[];
	onStage?: (stage: PrimitiveInstallStage) => void;
}

export interface UseStoreInstall {
	install: (entry: NgwaStoreEntry, scope: StoreInstallScope) => Promise<void>;
	update: (entry: NgwaStoreEntry) => Promise<void>;
	updateAll: (entries: NgwaStoreEntry[]) => Promise<void>;
	/** R57 flow 1: install a signed-catalog row and place it. */
	installPrimitive: (
		row: NgwaCatalogRow,
		scope: StoreInstallScope,
		ctx: PrimitiveInstallContext
	) => Promise<PrimitiveInstallOutcome>;
	/** R57 · Q4: move a catalog install to its catalog pin (never HEAD). */
	updatePrimitive: (row: NgwaCatalogRow) => Promise<void>;
	/** R57 flow 2: dry-run a URL (writes nothing). */
	resolveSource: typeof obaResolveSource;
	/** R57 flow 2: install exactly what was resolved, then place it. */
	installResolved: (
		resolved: ResolvedSource,
		scope: StoreInstallScope,
		ctx: PrimitiveInstallContext
	) => Promise<PrimitiveInstallOutcome>;
}

/**
 * Place a fetched target and its closure in `scope`, then settle the
 * already-satisfied deps (Q5). Shared by the catalog and URL paths.
 */
export async function placeInstalled(opts: {
	target: PrimitiveRef;
	installed: readonly PrimitiveRef[];
	satisfied: readonly PrimitiveRef[];
	scope: ClaudeStoreScope;
	vault: readonly ClaudeStoreEntry[];
}): Promise<PrimitiveInstallOutcome> {
	const deps: PrimitiveRef[] = [];
	// Deps first (they arrive deepest-first, the enable order), then the target.
	for (const dep of opts.installed) {
		await claudePrimitiveEnable(dep.kind as ClaudeStoreKind, dep.name, opts.scope);
		deps.push({ kind: dep.kind, name: dep.name });
	}
	await claudePrimitiveEnable(opts.target.kind as ClaudeStoreKind, opts.target.name, opts.scope);
	const placed = [{ kind: opts.target.kind, name: opts.target.name }, ...deps];

	const alsoEnabled: PrimitiveRef[] = [];
	const leftInPlace: PrimitiveRef[] = [];
	for (const dep of opts.satisfied) {
		const v = opts.vault.find((e) => e.kind === dep.kind && e.name === dep.name);
		// Reachable = already enabled in this scope, or personal (workspace),
		// which every project loads. A satisfied dep that is not a vault item
		// (a local copy) cannot be placed from here, so it stays where it is.
		const reachable = !v || v.enabledIn.includes(opts.scope) || v.enabledIn.includes('workspace');
		if (reachable) {
			leftInPlace.push({ kind: dep.kind, name: dep.name });
			continue;
		}
		await claudePrimitiveEnable(dep.kind as ClaudeStoreKind, dep.name, opts.scope);
		alsoEnabled.push({ kind: dep.kind, name: dep.name });
	}
	return { scope: opts.scope, placed, alsoEnabled, leftInPlace };
}

/** The pin a resolve result is installed against (N-C). */
export function resolvedPin(r: ResolvedSource): ObaPin {
	return { sha: r.sha, hash: r.hash };
}

export function useStoreInstall(): UseStoreInstall {
	const qc = useQueryClient();
	const indexUrl = useRegistryIndex().data?.indexUrl;
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const getDetail = cachedDetailGetter(qc, indexUrl);

	async function refresh() {
		// Settled, not success: a plan that failed midway may still have
		// installed its deps. The snapshot flips the row to installed / its new
		// version; the registry keys drop stale detail + plan caches; ['pkg']
		// is the kernel-status family the Installed surfaces read.
		await Promise.all([
			qc.invalidateQueries({ queryKey: ngwaSnapshotQueryKey }),
			qc.invalidateQueries({ queryKey: registryKeys.all }),
			qc.invalidateQueries({ queryKey: ['pkg'] }),
		]);
	}

	async function refreshVault() {
		// A primitive install / update changes the vault list, the on-disk scan
		// and (so the Installed tab sees it) the Ngwa snapshot.
		await Promise.all([
			qc.invalidateQueries({ queryKey: queryKeys.claudeStore.all }),
			qc.invalidateQueries({ queryKey: queryKeys.claudeConfig.all }),
			qc.invalidateQueries({ queryKey: ngwaSnapshotQueryKey }),
		]);
	}

	async function installOne(entry: NgwaStoreEntry, scope: PkgScopeWire | null, isUpdate: boolean) {
		const root = await getDetail(entry.registryEntry.name);
		if (isUpdate && entry.installedItem) {
			// Same pre-update capability diff as the batch updater (WP-41-F1):
			// the install records itself as approved, so stop here first.
			const review = await previewIncomingTrust(entry.installedItem.id, root, entry.latestVersion);
			if (review) {
				throw new Error(
					`${entry.displayName} ${entry.latestVersion} asks for new permissions — review them from the Installed tab before updating.`
				);
			}
		}
		await resolveAndInstall({ root, getDetail, version: entry.latestVersion, scope });
	}

	return {
		async install(entry, scope) {
			try {
				await installOne(entry, storeScopeWire(scope, activeProjectId), false);
			} finally {
				await refresh();
			}
		},
		async update(entry) {
			try {
				await installOne(entry, installedScopeWire(entry.installedItem?.scope), true);
			} finally {
				await refresh();
			}
		},
		async updateAll(entries) {
			const failed: string[] = [];
			try {
				// One failing pkg must not abort the rest (batch-updater rule).
				for (const entry of entries) {
					try {
						await installOne(entry, installedScopeWire(entry.installedItem?.scope), true);
					} catch (e) {
						failed.push(`${entry.displayName}: ${e instanceof Error ? e.message : String(e)}`);
					}
				}
			} finally {
				await refresh();
			}
			if (failed.length) {
				throw new Error(
					`${failed.length} of ${entries.length} update${entries.length === 1 ? '' : 's'} failed — ${failed.join('; ')}`
				);
			}
		},
		async installPrimitive(row, scope, ctx) {
			const wire = primitiveScopeWire(scope, activeProjectId);
			const e = row.entry;
			try {
				ctx.onStage?.('fetch');
				const result = await obaInstallWithDeps(
					e.kind,
					e.name,
					e.source,
					e.url,
					catalogRefs(ctx.catalog),
					catalogGitRef(e),
					true,
					catalogPin(e)
				);
				ctx.onStage?.('place');
				return await placeInstalled({
					target: { kind: result.target.kind, name: result.target.name },
					installed: result.installed.map((d) => ({ kind: d.kind, name: d.name })),
					satisfied: result.alreadySatisfied,
					scope: wire,
					vault: ctx.vault,
				});
			} finally {
				await refreshVault();
			}
		},
		async updatePrimitive(row) {
			const pin = catalogPin(row.entry);
			if (!pin) throw new Error(`${row.name} has no catalog pin to move to`);
			try {
				await obaUpdate(row.storeKind, row.name, pin);
			} finally {
				await refreshVault();
			}
		},
		resolveSource: obaResolveSource,
		async installResolved(resolved, scope, ctx) {
			const wire = primitiveScopeWire(scope, activeProjectId);
			const pin = resolvedPin(resolved);
			try {
				ctx.onStage?.('fetch');
				let installed: PrimitiveRef[] = [];
				let satisfied: PrimitiveRef[] = [];
				if (resolved.requires.length > 0) {
					const result = await obaInstallWithDeps(
						resolved.kind,
						resolved.name,
						resolved.source,
						resolved.url,
						catalogRefs(ctx.catalog),
						resolved.source === 'git' ? resolved.ref : null,
						false,
						pin
					);
					installed = result.installed.map((d) => ({ kind: d.kind, name: d.name }));
					satisfied = result.alreadySatisfied;
				} else if (resolved.source === 'npx') {
					await obaInstallNpx('skill', resolved.name, resolved.url, false, pin);
				} else {
					await obaInstallGit(resolved.kind, resolved.name, resolved.url, resolved.ref, false, pin);
				}
				ctx.onStage?.('place');
				return await placeInstalled({
					target: { kind: resolved.kind, name: resolved.name },
					installed,
					satisfied,
					scope: wire,
					vault: ctx.vault,
				});
			} finally {
				await refreshVault();
			}
		},
	};
}
