// Phase 5 — Onboarding pkg catalog.
//
// The list of pkgs the wizard shows the user during step 4. Each entry is a
// `ManifestLike` projection (so it threads through the connector resolver
// identically to an installed pkg's real manifest) plus presentational
// metadata for the picker card.
//
// This is intentionally a static list: the wizard runs before the user has
// configured any registry, and "browse the registry" is a post-onboarding
// flow that lives on `/install`. New pkgs added to the registry will appear
// on `/install` without needing to ship a shell update.
//
// Only offer what exists in the public registry and works on a fresh install.
// Apps the registry holds back (`visibility: "hidden"`) are not listed here
// and are never pre-selected. Of the apps that are listed, only Tasks is
// pre-selected; Sales is listed but off by default.

import type { ManifestLike } from './connectors';

export type CatalogIconKey =
	| 'studio'
	| 'tasks'
	| 'mail'
	| 'outbound'
	| 'content'
	| 'sales'
	| 'files'
	| 'engine';

export type CatalogTrafficLight = 'local-only' | 'needs-cloud' | 'engine';

export interface OnboardingPkgEntry {
	manifest: ManifestLike;
	display: string;
	summary: string;
	version: string;
	icon: CatalogIconKey;
	/** Coarse classification used by the filter bar above the pkg grid. */
	bucket: CatalogTrafficLight;
	/** Pre-selected by default on first run. */
	defaultSelected: boolean;
	/** Selection cannot be toggled off (e.g. the engine pkg the user
	 *  picked in step 2). */
	pinned?: boolean;
	/** Approximate download size, surfaced in the footer total. */
	sizeMb?: number;
}

// ──────────────────────────────────────────────────────────────────────
// Canonical entries
// ──────────────────────────────────────────────────────────────────────

export const ONBOARDING_PKG_CATALOG: readonly OnboardingPkgEntry[] = Object.freeze([
	{
		manifest: {
			id: 'com.ikenga.studio',
			name: 'Studio',
			version: '0.4.0',
			permissions: { 'vault.keys': [] },
		},
		display: 'Studio',
		summary: 'Storyboard, hyperframes, and Remotion-powered video for releases.',
		version: '0.4.0',
		icon: 'studio',
		bucket: 'local-only',
		defaultSelected: true,
		sizeMb: 14,
	},
	{
		manifest: {
			id: 'com.ikenga.tasks',
			name: 'Tasks',
			version: '0.8.4',
			permissions: { 'vault.keys': [] },
		},
		display: 'Tasks',
		summary: 'Task list, agenda and triage for you and your agents, kept on this computer.',
		version: '0.8.4',
		icon: 'tasks',
		bucket: 'local-only',
		defaultSelected: true,
		sizeMb: 0.4,
	},
	{
		manifest: {
			id: 'com.ikenga.sales',
			name: 'Sales',
			version: '0.4.1',
			permissions: { 'vault.keys': [] },
		},
		display: 'Sales',
		summary: 'Deal pipeline, forecast and won deals, kept on this computer.',
		version: '0.4.1',
		icon: 'sales',
		bucket: 'local-only',
		defaultSelected: false,
		sizeMb: 0.3,
	},
	{
		manifest: {
			id: 'com.ikenga.engine-claude-code',
			name: 'Engine: Claude Code',
			version: '0.5.0',
			permissions: { 'vault.keys': [] },
		},
		display: 'Engine: Claude Code',
		summary: 'Default engine adapter. Updates independently of the shell.',
		version: '0.5.0',
		icon: 'engine',
		bucket: 'engine',
		defaultSelected: true,
		pinned: false,
		sizeMb: 8.0,
	},
]);

export const BUCKET_LABEL: Record<CatalogTrafficLight, string> = {
	'local-only': 'Local-only',
	'needs-cloud': 'Needs cloud',
	engine: 'Engine pkgs',
};

export function defaultSelectedIds(): string[] {
	return ONBOARDING_PKG_CATALOG.filter((p) => p.defaultSelected).map((p) => p.manifest.id);
}

export function findCatalogEntry(id: string): OnboardingPkgEntry | undefined {
	return ONBOARDING_PKG_CATALOG.find((p) => p.manifest.id === id);
}

export function countByBucket(): Record<CatalogTrafficLight | 'all', number> {
	const out: Record<CatalogTrafficLight | 'all', number> = {
		all: ONBOARDING_PKG_CATALOG.length,
		'local-only': 0,
		'needs-cloud': 0,
		engine: 0,
	};
	for (const entry of ONBOARDING_PKG_CATALOG) {
		out[entry.bucket]++;
	}
	return out;
}
