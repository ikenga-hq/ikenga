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

import { useQueryClient } from '@tanstack/react-query';
import type { NgwaScope } from '@ikenga/contract';
import type { NgwaStoreEntry } from '@/lib/ngwa/enrichment';
import { ngwaSnapshotQueryKey } from '@/lib/ngwa/use-ngwa-snapshot';
import {
	cachedDetailGetter,
	previewIncomingTrust,
	resolveAndInstall,
} from '@/lib/registry/install-plan';
import { registryKeys, useRegistryIndex } from '@/lib/registry/use-registry';
import { useShellStore } from '@/lib/shell/shell-store';
import type { PkgScopeWire } from '@/lib/tauri-cmd';

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

export interface UseStoreInstall {
	install: (entry: NgwaStoreEntry, scope: StoreInstallScope) => Promise<void>;
	update: (entry: NgwaStoreEntry) => Promise<void>;
	updateAll: (entries: NgwaStoreEntry[]) => Promise<void>;
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
	};
}
