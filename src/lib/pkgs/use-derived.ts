// Central derive hook for the unified pkg surface.
//
// Reads the existing queries (kernel status, registry index, trust list,
// violations list, manifests) and returns a single object that mirrors the
// `D` shape from the design artifact at
// design/shell/concepts/04-pkgs/04-package-manager/_data.js::derive().
//
// One hook → all surface chrome can consume it without each component
// rebuilding the queries.

import { useMemo } from 'react';
import { useQuery } from '@tanstack/react-query';
import {
	pkgKernelStatus,
	pkgPermissionViolationsList,
	pkgPreviewManifest,
	pkgTrustList,
	type PkgInstalledSummary,
	type PkgManifestPreview,
	type PkgPermissionViolation,
	type PkgTrustEntry,
} from '@/lib/tauri-cmd';
import { useRegistryIndex, type RegistryEntry } from '@/lib/registry/use-registry';
import { entryMatchesPkgId } from '@/lib/registry/use-updates-available';
import { semverCompare } from '@ikenga/registry-client';

export type RowOrigin = 'builtin' | 'engine' | 'user' | 'registry';
export type RowState = 'running' | 'idle' | 'disabled' | 'not-installed';

/**
 * Reference to one screenshot, decoupled from how the bytes are fetched.
 *
 * - Installed pkgs: `kind === 'installed-pkg'`. `pkgId + path` identifies the
 *   image; the bytes come from the `pkg_screenshot` Tauri command and need
 *   a Query roundtrip (see usePkgScreenshot in the v2 atoms). `src` is empty.
 * - Registry pkgs: `kind === 'url'`. `src` is an absolute https:// URL the
 *   webview can load directly. No fetch needed.
 */
export type PkgScreenshotRef =
	| { kind: 'installed-pkg'; pkgId: string; path: string; caption: string | null; src: '' }
	| { kind: 'url'; src: string; caption: string | null };

/** Normalized row used across the v2 surface. */
export interface PkgRowV2 {
	id: string;
	name: string;
	version: string;
	origin: RowOrigin;
	kind: string; // engine | ui | mcp | skill | embedded | …
	state: RowState;
	enabled: boolean;
	desc: string;
	installPath: string;
	installedAt: number | null;
	/** Latest version known to the registry, if newer than `version`. */
	latest: string | null;
	scopes: string[];
	routes: string[];
	sidecars: string[];
	trust: PkgTrustEntry | null;
	/** Recorded permission violations for this pkg. */
	violations: PkgPermissionViolation[];
	/** Pkg-declared preview screenshots. Empty array if none declared. */
	screenshots: PkgScreenshotRef[];
	/** Raw installed summary (only populated for non-registry rows). */
	installed: PkgInstalledSummary | null;
	/** Raw manifest preview (only populated when fetched). */
	manifest: PkgManifestPreview | null;
	/** Raw registry entry (only populated for registry-only rows). */
	registryEntry: RegistryEntry | null;
}

export interface DerivedPkgs {
	rows: PkgRowV2[];
	installed: PkgRowV2[];
	registry: PkgRowV2[];
	updates: PkgRowV2[];
	trust: PkgRowV2[];
	violations: PkgRowV2[];
	builtin: PkgRowV2[];
	engine: PkgRowV2[];
	user: PkgRowV2[];
	sidecarsRunning: number;
	isLoading: boolean;
	error: string | null;
}

const EMPTY: DerivedPkgs = {
	rows: [],
	installed: [],
	registry: [],
	updates: [],
	trust: [],
	violations: [],
	builtin: [],
	engine: [],
	user: [],
	sidecarsRunning: 0,
	isLoading: false,
	error: null,
};

function classifyOrigin(p: PkgInstalledSummary, kind: string): RowOrigin {
	if (kind === 'engine') return 'engine';
	if (p.source?.kind === 'builtin') return 'builtin';
	return 'user';
}

function summarizeScopes(perms: Record<string, unknown> | undefined): string[] {
	if (!perms) return [];
	const out: string[] = [];
	for (const [k, v] of Object.entries(perms)) {
		if (Array.isArray(v) && v.length) {
			for (const item of v as unknown[]) {
				out.push(`${k}:${String(item)}`);
			}
		} else if (v === true) {
			out.push(k);
		}
	}
	return out;
}

function summarizeRoutes(manifest: PkgManifestPreview | null): string[] {
	if (!manifest?.ui?.routes) return [];
	return manifest.ui.routes.map((r) => r.path.replace(/^\//, ''));
}

function summarizeSidecars(manifest: PkgManifestPreview | null): string[] {
	if (!manifest) return [];
	const out: string[] = [];
	if (manifest.sidecars) {
		for (const s of manifest.sidecars) out.push(s.name);
	}
	if (manifest.mcp) {
		for (const m of manifest.mcp) out.push(m.name);
	}
	return out;
}

function descriptionFor(manifest: PkgManifestPreview | null): string {
	if (!manifest) return '';
	const summary = (manifest.permissions as { summary?: unknown } | undefined)?.summary;
	if (typeof summary === 'string' && summary.trim()) return summary;
	if (typeof manifest['description'] === 'string') return manifest['description'] as string;
	if (manifest.kind) return `${manifest.kind} pkg`;
	return '';
}

/**
 * Pure derivation of {@link DerivedPkgs} from raw query data. Extracted
 * from {@link usePkgsDerived} so it can be tested without a QueryClient.
 */
export interface DeriveInputs {
	statusData: { installed: PkgInstalledSummary[] } | undefined;
	statusLoading?: boolean;
	statusError?: Error | null;
	trustData?: PkgTrustEntry[];
	trustLoading?: boolean;
	trustError?: Error | null;
	violationsData?: PkgPermissionViolation[];
	violationsError?: Error | null;
	manifestsData?: Record<string, PkgManifestPreview | { _error: string }>;
	registryEntries?: RegistryEntry[];
}

export function deriveFromQueries(inputs: DeriveInputs): DerivedPkgs {
	const {
		statusData,
		statusLoading = false,
		statusError = null,
		trustData = [],
		trustLoading = false,
		trustError = null,
		violationsData = [],
		violationsError = null,
		manifestsData,
		registryEntries = [],
	} = inputs;

	const error = statusError?.message ?? trustError?.message ?? violationsError?.message ?? null;

	if (!statusData) {
		return { ...EMPTY, isLoading: statusLoading, error };
	}

	const trustByPkg = new Map<string, PkgTrustEntry>();
	for (const t of trustData) trustByPkg.set(t.pkg_id, t);

	const violationsByPkg = new Map<string, PkgPermissionViolation[]>();
	for (const v of violationsData) {
		const list = violationsByPkg.get(v.pkg_id) ?? [];
		list.push(v);
		violationsByPkg.set(v.pkg_id, list);
	}

	const installedRows: PkgRowV2[] = (statusData.installed ?? []).map((s) => {
		const m = manifestsData?.[s.install_path];
		const manifest = m && !('_error' in m) ? (m as PkgManifestPreview) : null;
		const kind = manifest?.kind ?? 'ui';
		const origin = classifyOrigin(s, kind);
		const sidecars = summarizeSidecars(manifest);
		const state: RowState = !s.enabled ? 'disabled' : sidecars.length ? 'running' : 'idle';
		const screenshots: PkgScreenshotRef[] = (manifest?.screenshots ?? []).map((shot) => ({
			kind: 'installed-pkg' as const,
			pkgId: s.id,
			path: shot.path,
			caption: shot.caption ?? null,
			src: '' as const,
		}));
		return {
			id: s.id,
			name: manifest?.name ?? s.id,
			version: s.version,
			origin,
			kind,
			state,
			enabled: s.enabled,
			desc: descriptionFor(manifest),
			installPath: s.install_path,
			installedAt: s.installed_at,
			latest: null, // filled below from registry
			scopes: summarizeScopes(manifest?.permissions),
			routes: summarizeRoutes(manifest),
			sidecars,
			trust: trustByPkg.get(s.id) ?? null,
			violations: violationsByPkg.get(s.id) ?? [],
			screenshots,
			installed: s,
			manifest,
			registryEntry: null,
		};
	});

	// Cross-reference registry for updates + dangling registry entries. The
	// kernel records reverse-DNS ids (`com.ikenga.mcp-iyke`) while the registry
	// index lists npm names (`@ikenga/mcp-iyke`); `entryMatchesPkgId` bridges
	// the two so installed pkgs are deduped from the "Available in registry"
	// group and their available-update version is detected. Same matcher the
	// activity-bar badge uses, so both surfaces agree.
	//
	// Updates are only offered for registry-source installs. Builtins ship
	// with the shell (they update on app update), and dev/local installs point
	// at a working tree the registry must not clobber — and the kernel refuses
	// a same-id install at a different path anyway ("already installed from …
	// — uninstall first"), which used to abort the whole batch silently.
	for (const row of installedRows) {
		if (row.installed?.source.kind !== 'registry') continue;
		const match = registryEntries.find((e) => entryMatchesPkgId(e.name, row.id));
		if (match && match.latest && semverCompare(row.version, match.latest) < 0) {
			row.latest = match.latest;
			// Carry the matching registry entry so the install sheet can resolve
			// the update's signed dep-plan — same path a fresh registry install
			// takes. Without this the sheet has no detail to fetch.
			row.registryEntry = match;
		}
	}
	const registryRows: PkgRowV2[] = registryEntries
		// Hide dev/test fixtures + scaffolds from the default catalog. They stay
		// installable by exact name and keep update detection (the installedRows
		// matcher above is intentionally unfiltered).
		.filter((e) => (e as { visibility?: string }).visibility !== 'hidden')
		.filter((e) => !installedRows.some((r) => entryMatchesPkgId(e.name, r.id)))
		.map((e) => {
			const heroShot = (e as { screenshot?: string }).screenshot ?? null;
			const screenshots: PkgScreenshotRef[] = heroShot
				? [{ kind: 'url' as const, src: heroShot, caption: null }]
				: [];
			return {
				id: e.name,
				name: e.name,
				version: e.latest,
				origin: 'registry' as const,
				kind: e.kind ?? 'ui',
				state: 'not-installed' as const,
				enabled: false,
				desc: e.description ?? '',
				installPath: '(not installed)',
				installedAt: null,
				latest: e.latest,
				scopes: [],
				routes: [],
				sidecars: [],
				trust: null,
				violations: [],
				screenshots,
				installed: null,
				manifest: null,
				registryEntry: e,
			};
		});

	const rows = [...installedRows, ...registryRows];
	const updates = installedRows.filter((r) => r.latest && r.latest !== r.version);
	const trust = installedRows.filter((r) => r.trust?.state === 'needs_approval');
	const violationsRows = installedRows.filter((r) => r.violations.length > 0);
	const builtin = installedRows.filter((r) => r.origin === 'builtin');
	const engine = installedRows.filter((r) => r.origin === 'engine');
	const user = installedRows.filter((r) => r.origin === 'user');
	const sidecarsRunning = installedRows.reduce(
		(n, r) => n + (r.state === 'running' ? r.sidecars.length : 0),
		0
	);

	return {
		rows,
		installed: installedRows,
		registry: registryRows,
		updates,
		trust,
		violations: violationsRows,
		builtin,
		engine,
		user,
		sidecarsRunning,
		isLoading: statusLoading || trustLoading,
		error,
	};
}

/** `usePkgsDerived()`'s return type — `DerivedPkgs` plus the raw registry
 *  query's error/refetch. Kept off `DerivedPkgs` itself (and off
 *  `deriveFromQueries`) so the pure-function shape existing fixtures build
 *  against (`pkgs-surface.test.tsx::makeDerived`) doesn't grow two fields it
 *  never needed. WP-43 added this pair for the Ngwa Store `offline` state —
 *  "everything installed still runs, only browsing/installing needs the
 *  network" is exactly what a registry-only error means here. */
export interface UsePkgsDerivedResult extends DerivedPkgs {
	registryError: Error | null;
	retryRegistry: () => void;
}

export function usePkgsDerived(): UsePkgsDerivedResult {
	const status = useQuery({
		queryKey: ['pkg', 'kernel-status'],
		queryFn: pkgKernelStatus,
		refetchOnWindowFocus: false,
	});

	const trustList = useQuery({
		queryKey: ['pkg', 'trust-list'],
		queryFn: pkgTrustList,
		refetchOnWindowFocus: false,
	});

	const violations = useQuery({
		queryKey: ['pkg', 'violations-list'],
		queryFn: () => pkgPermissionViolationsList(undefined, 1000),
		refetchOnWindowFocus: false,
	});

	const installPaths = (status.data?.installed ?? []).map((p) => p.install_path);
	const manifests = useQuery({
		enabled: installPaths.length > 0,
		queryKey: ['pkg', 'manifests', installPaths.join('|')],
		staleTime: Infinity,
		queryFn: async (): Promise<Record<string, PkgManifestPreview | { _error: string }>> => {
			const out: Record<string, PkgManifestPreview | { _error: string }> = {};
			await Promise.all(
				installPaths.map(async (path) => {
					try {
						out[path] = await pkgPreviewManifest(path);
					} catch (e) {
						out[path] = { _error: (e as Error).message ?? String(e) };
					}
				})
			);
			return out;
		},
	});

	const registry = useRegistryIndex();

	const derived = useMemo<DerivedPkgs>(
		() =>
			deriveFromQueries({
				statusData: status.data,
				statusLoading: status.isLoading,
				statusError: status.error as Error | null,
				trustData: trustList.data ?? undefined,
				trustLoading: trustList.isLoading,
				trustError: trustList.error as Error | null,
				violationsData: violations.data ?? undefined,
				violationsError: violations.error as Error | null,
				manifestsData: manifests.data,
				registryEntries: registry.data?.index?.pkgs ?? [],
			}),
		[
			status.data,
			status.isLoading,
			status.error,
			trustList.data,
			trustList.isLoading,
			trustList.error,
			violations.data,
			violations.error,
			manifests.data,
			registry.data,
		]
	);

	return {
		...derived,
		registryError: (registry.error as Error | null) ?? null,
		retryRegistry: () => void registry.refetch(),
	};
}
