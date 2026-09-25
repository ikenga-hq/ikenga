// Batch pkg-update mutation. Given a set of installed rows that each carry a
// `registryEntry` (populated by use-derived when a newer version exists),
// resolve each pkg's signed dep-plan and re-install over the existing path.
// The kernel treats a same-path re-install as an in-place upgrade —
// unregister → re-register → emit `pkg-reloaded` — so mounted iframes remount
// without a shell restart.
//
// Powers both the "Update all" button on the /packages surface and the
// background auto-updater mounted in the workspace.

import { useMutation, useQueryClient } from '@tanstack/react-query';
import { fetchPkgDetail, resolveInstallPlan, type PkgDetail } from '@/lib/registry/client';
import { registryKeys, useRegistryIndex } from '@/lib/registry/use-registry';
import {
	pkgInstallFromRegistry,
	pkgTrustPreviewIncoming,
	type PkgTrustReview,
} from '@/lib/tauri-cmd';
import type { PkgRowV2 } from './use-derived';

/**
 * Read the incoming manifest's `capabilities` + `permissions` blocks off a
 * fetched `PkgDetail`. The registry's per-pkg detail file IS manifest-shaped
 * (same pattern `pkg-install-sheet.tsx::extractElevatedCaps` relies on) —
 * there's no separate "capabilities preview" endpoint, so this is the data
 * source for the pre-install diff below.
 */
function incomingCapabilityFields(detail: PkgDetail): {
	version: string;
	capabilitiesJson?: string;
	permissionsJson?: string;
} {
	const d = detail as unknown as {
		version?: string;
		capabilities?: unknown;
		permissions?: unknown;
	};
	return {
		version: d.version ?? '',
		capabilitiesJson: d.capabilities !== undefined ? JSON.stringify(d.capabilities) : undefined,
		permissionsJson: d.permissions !== undefined ? JSON.stringify(d.permissions) : undefined,
	};
}

export interface UpdateProgress {
	/** Index of the pkg currently being updated (0-based). */
	done: number;
	/** Total pkgs in this batch. */
	total: number;
	/** Display name of the pkg currently being updated. */
	current: string;
}

export interface UpdatePkgsArgs {
	rows: PkgRowV2[];
	onProgress?: (p: UpdateProgress) => void;
	/**
	 * Row ids to install even though their incoming version requests a new
	 * capability — set for a row the user already approved via the trust
	 * review modal (WP-41-F1). Every other row still gets the pre-install
	 * capability diff and, if it finds a new capability, is parked into
	 * `needsApproval` instead of being installed.
	 */
	approvedIds?: ReadonlySet<string>;
}

/** One pkg that failed to update; the rest of the batch still ran. */
export interface UpdateFailure {
	id: string;
	name: string;
	error: string;
}

export interface UpdatePkgsResult {
	/** Count of pkgs updated successfully. */
	updated: number;
	/** Per-pkg failures. A failure no longer aborts the batch — surface these
	 *  to the user (they used to be swallowed entirely, which looked like
	 *  "progress bar ran, nothing happened"). */
	failed: UpdateFailure[];
	/**
	 * Rows the pre-install capability diff (`pkgTrustPreviewIncoming` —
	 * WP-41-F1) flagged as requesting a new capability/permission beyond
	 * what's already approved. These are NOT installed — `install_from_path`
	 * writes its implicit-approval snapshot unconditionally at install time,
	 * so the only way to stop for review is to decide before calling it.
	 * The caller re-runs the batch with the row's id in `approvedIds` (after
	 * the user approves via the trust review modal) to actually install it.
	 * Empty on any batch that didn't trigger a capability diff — existing
	 * callers that ignore this field see unchanged behavior.
	 */
	needsApproval: PkgTrustReview[];
}

export function useUpdatePkgs() {
	const qc = useQueryClient();
	const indexQuery = useRegistryIndex();
	const indexUrl = indexQuery.data?.indexUrl;

	// Session-cached detail fetch, shared with the resolver. Mirrors the
	// getDetail in useInstallPlanResolver so any pkg the user already inspected
	// won't refetch.
	const getDetail = async (name: string): Promise<PkgDetail> => {
		const cached = qc.getQueryData<PkgDetail>(registryKeys.detail(name));
		if (cached) return cached;
		if (!indexUrl) throw new Error('registry index not available');
		const detail = await fetchPkgDetail(indexUrl, { name });
		qc.setQueryData(registryKeys.detail(name), detail);
		return detail;
	};

	return useMutation({
		mutationFn: async ({
			rows,
			onProgress,
			approvedIds,
		}: UpdatePkgsArgs): Promise<UpdatePkgsResult> => {
			const targets = rows.filter((r) => r.registryEntry && r.latest && r.latest !== r.version);
			let done = 0;
			const failed: UpdateFailure[] = [];
			const needsApproval: PkgTrustReview[] = [];
			for (const row of targets) {
				onProgress?.({ done: done + failed.length, total: targets.length, current: row.name });
				try {
					const root = await getDetail(row.registryEntry!.name);

					// Decide BEFORE installing (WP-41-F1): `install_from_path`
					// unconditionally records its own install as implicitly
					// approved, so checking for a capability diff after the
					// fact (the old approach) never finds anything to flag.
					// Skip the check for a row the caller already routed
					// through the trust review modal and got approved.
					if (!approvedIds?.has(row.id)) {
						const incoming = incomingCapabilityFields(root);
						const review = await pkgTrustPreviewIncoming({
							pkgId: row.id,
							manifestVersion: incoming.version || row.latest || row.version,
							capabilitiesJson: incoming.capabilitiesJson,
							permissionsJson: incoming.permissionsJson,
						});
						if (review) {
							needsApproval.push(review);
							continue;
						}
					}

					const plan = await resolveInstallPlan(root, getDetail);
					for (const step of plan) {
						await pkgInstallFromRegistry({
							tarball: step.tarball,
							integrity: step.integrity,
							pkgId: step.pkgId,
							sourceUrl: step.tarball,
							// Signed-index publisher key (WP-06 will populate it;
							// undefined today -> installs untrusted-for-elevated).
							publisherKey: (step as { publisherKey?: string | null }).publisherKey ?? undefined,
						});
					}
					done += 1;
				} catch (e) {
					// One bad pkg must not abort the rest of the batch — record it
					// and keep going; callers render the failures.
					failed.push({
						id: row.id,
						name: row.name,
						error: e instanceof Error ? e.message : String(e),
					});
				}
			}
			onProgress?.({ done: done + failed.length, total: targets.length, current: '' });

			return { updated: done, failed, needsApproval };
		},
		onSettled: () => {
			// Settled, not success: even a batch that ends with failures may have
			// installed some pkgs before the failing row — refetch regardless.
			void qc.invalidateQueries({ queryKey: ['pkg'] });
			void qc.invalidateQueries({ queryKey: registryKeys.all });
		},
	});
}
