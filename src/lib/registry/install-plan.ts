// Shared registry install path: resolve a pkg's signed dep plan and walk it
// through `pkgInstallFromRegistry`, one Tauri call per step.
//
// Used by the v2 install sheet (`components/pkg/v2/pkg-install-sheet.tsx`),
// the batch updater (`lib/pkgs/use-update-pkgs.ts`) and the Ngwa Store
// (`lib/ngwa/use-store-install.ts`). An update is the same call: the kernel
// treats a registry install over an existing pkg dir as an in-place upgrade
// (stage → swap → unregister → re-register → `pkg-reloaded`).

import type { QueryClient } from '@tanstack/react-query';
import {
	pkgInstallFromRegistry,
	pkgTrustPreviewIncoming,
	type PkgInstallFromRegistryArgs,
	type PkgScopeWire,
	type PkgTrustReview,
} from '@/lib/tauri-cmd';
import { fetchPkgDetail, resolveInstallPlan, type InstallStep, type PkgDetail } from './client';
import { registryKeys } from './use-registry';

export interface InstallPlanProgress {
	/** Steps finished so far. */
	done: number;
	total: number;
	/** npm name of the step in flight ('' once finished). */
	current: string;
}

/** The `pkgInstallFromRegistry` args for one resolved plan step. */
export function registryInstallArgs(step: InstallStep): PkgInstallFromRegistryArgs {
	return {
		tarball: step.tarball,
		integrity: step.integrity,
		pkgId: step.pkgId,
		sourceUrl: step.tarball,
		// Publisher key from the signed index entry. The registry schema doesn't
		// carry per-pkg publisher keys yet (WP-06), so this reads as undefined
		// today -> installs as untrusted (no elevated host caps) until keys land.
		// Sourced defensively so no call-site change is needed then.
		publisherKey: (step as { publisherKey?: string | null }).publisherKey ?? undefined,
	};
}

/**
 * Install every step of an already-resolved plan, in order (deps first — the
 * resolver's order). `scope` null/undefined = the kernel's default, the active
 * project. Stops at the first failing step and rethrows.
 */
export interface RunInstallPlanOptions {
	scope?: PkgScopeWire | null;
	onProgress?: (p: InstallPlanProgress) => void;
	/** Tags every step's progress events (and is the cancel handle). */
	installId?: string;
	/** Aborted = stop before the next step; the step in flight is cancelled
	 *  separately through `pkgInstallCancel(installId)`. */
	signal?: AbortSignal;
	/** Called as each step starts, with its index and pkg id. */
	onStep?: (index: number, total: number, pkgId: string) => void;
}

/** The error a run stops with when its signal is aborted between steps; the
 *  same text the installer uses, so both classify as cancelled. */
export const INSTALL_CANCELLED = 'install cancelled';

export async function runInstallPlan(
	plan: InstallStep[],
	opts: RunInstallPlanOptions = {}
): Promise<number> {
	let done = 0;
	for (const step of plan) {
		if (opts.signal?.aborted) throw new Error(INSTALL_CANCELLED);
		opts.onProgress?.({ done, total: plan.length, current: step.name });
		opts.onStep?.(done, plan.length, step.pkgId);
		const args = registryInstallArgs(step);
		if (opts.installId) args.installId = opts.installId;
		if (opts.scope) await pkgInstallFromRegistry(args, opts.scope);
		else await pkgInstallFromRegistry(args);
		done += 1;
	}
	opts.onProgress?.({ done, total: plan.length, current: '' });
	return done;
}

/** Resolve `root`'s dep plan at `version` (or latest) and install it. */
export async function resolveAndInstall(
	opts: {
		root: PkgDetail;
		getDetail: (name: string) => Promise<PkgDetail>;
		version?: string;
	} & RunInstallPlanOptions
): Promise<InstallStep[]> {
	const { root, getDetail, version, ...run } = opts;
	const plan = await resolveInstallPlan(root, getDetail, version);
	await runInstallPlan(plan, run);
	return plan;
}

/**
 * Session-cached detail getter shared with `useInstallPlanResolver`: any pkg
 * already inspected isn't refetched.
 */
export function cachedDetailGetter(
	qc: QueryClient,
	indexUrl: string | undefined
): (name: string) => Promise<PkgDetail> {
	return async (name) => {
		const cached = qc.getQueryData<PkgDetail>(registryKeys.detail(name));
		if (cached) return cached;
		if (!indexUrl) throw new Error('registry index not available');
		const detail = await fetchPkgDetail(indexUrl, { name });
		qc.setQueryData(registryKeys.detail(name), detail);
		return detail;
	};
}

/**
 * Pre-update capability diff (WP-41-F1). `install_from_path` records its own
 * install as implicitly approved, so a new capability has to be caught before
 * installing. Returns the review when the incoming version asks for more than
 * was approved, else null.
 */
export async function previewIncomingTrust(
	pkgId: string,
	root: PkgDetail,
	fallbackVersion: string
): Promise<PkgTrustReview | null> {
	// The registry's per-pkg detail file IS manifest-shaped (same pattern
	// `pkg-install-sheet.tsx::extractElevatedCaps` relies on).
	const d = root as unknown as { version?: string; capabilities?: unknown; permissions?: unknown };
	return pkgTrustPreviewIncoming({
		pkgId,
		manifestVersion: d.version || fallbackVersion,
		capabilitiesJson: d.capabilities !== undefined ? JSON.stringify(d.capabilities) : undefined,
		permissionsJson: d.permissions !== undefined ? JSON.stringify(d.permissions) : undefined,
	});
}
