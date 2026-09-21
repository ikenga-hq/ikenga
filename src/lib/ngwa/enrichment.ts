// Frontend enrichment layer for Ngwa equipment (WP-15 / Gate G-NGWA-ITEM §13).
//
// Cross-references the snapshot from Rust with the HTTP-fetched registry index:
// 1. Fills `latest_version` from registry.
// 2. Sets `state: 'update'` when installed version < registry latest.
// 3. Implements Gate §5 trust facet derivation (builtin · signed · unsigned · review).
// 4. Implements Gate §2 usage null semantics: null -> "—", measured 0 -> "0 sessions".
// 5. Generates available store items for non-installed registry entries.

import type { NgwaItem, NgwaTrust, NgwaUsage, NgwaKind } from '@ikenga/contract';
import type { RegistryEntry } from '@/lib/registry/use-registry';
import { entryMatchesPkgId } from '@/lib/registry/use-updates-available';
import { semverCompare } from '@ikenga/registry-client';

export type TrustFacetValue = 'builtin' | 'signed' | 'unsigned' | 'review';

/**
 * Resolves the D-02 Trust facet value for an item per Gate §5:
 * - builtin: auto_trusted === true (builtin / dev)
 * - signed: signed === true (manifest signature present)
 * - unsigned: signed === false && state !== 'needs_approval'
 * - review: state === 'needs_approval' || review_pending === true
 */
export function resolveTrustFacet(trust: NgwaTrust): TrustFacetValue {
	if (trust.state === 'needs_approval' || trust.review_pending) return 'review';
	if (trust.auto_trusted) return 'builtin';
	if (trust.signed) return 'signed';
	return 'unsigned';
}

/**
 * Enriches a single NgwaItem with frontend registry data.
 * The Rust snapshot emits `latest_version: null` (Gate §13); this function
 * fills `latest_version` and updates `state` to `'update'` if a newer version is found.
 */
export function enrichNgwaItem(item: NgwaItem, registryEntries: RegistryEntry[]): NgwaItem {
	const match = registryEntries.find(
		(e) =>
			entryMatchesPkgId(e.name, item.id) ||
			entryMatchesPkgId(e.name, item.name) ||
			e.name === item.name
	);

	if (!match || !match.latest) {
		return item;
	}

	const latest = match.latest;
	const hasUpdate = item.version !== null && semverCompare(item.version, latest) < 0;

	return {
		...item,
		latest_version: latest,
		state: hasUpdate ? 'update' : item.state,
	};
}

/**
 * Enriches all snapshot items with registry index data.
 */
export function enrichNgwaItems(
	items: NgwaItem[],
	registryEntries: RegistryEntry[]
): NgwaItem[] {
	if (!registryEntries.length) return items;
	return items.map((it) => enrichNgwaItem(it, registryEntries));
}

/**
 * Formats usage count for display in the equipment table.
 * INVARIANT: usage === null renders strictly as "—", NEVER as "0".
 * A measured zero renders as "0 sessions".
 */
export function formatUsageDisplay(usage: NgwaUsage | null): string {
	if (usage === null) return '—';
	const count = usage.count_7d ?? usage.count_30d;
	if (count === null) return '—';
	return `${count} sessions`;
}

/**
 * Honest tooltip text for an item's usage metrics.
 * Explicitly mentions that tokens include cache-read tokens (DEC-27 / Round 15).
 */
export function formatUsageTooltip(usage: NgwaUsage | null): string {
	if (usage === null) {
		return 'Never measured (unmeasured equipment reads “—” rather than a guess)';
	}
	const count7d = usage.count_7d !== null ? `${usage.count_7d} sessions` : '—';
	const count30d = usage.count_30d !== null ? `${usage.count_30d} sessions` : '—';
	const tokens =
		usage.tokens_30d !== null
			? `${usage.tokens_30d.toLocaleString()} tokens in 30d (includes cache-read tokens)`
			: 'No token telemetry for this kind';

	const lines = [
		`Usage source: ${usage.source}`,
		`7 days: ${count7d}`,
		`30 days: ${count30d}`,
		tokens,
	];
	if (usage.last_used_ms) {
		lines.push(`Last used: ${new Date(usage.last_used_ms).toLocaleString()}`);
	}
	return lines.join('\n');
}

/**
 * Normalized store entry for the Store tab.
 */
export interface NgwaStoreEntry {
	id: string;
	name: string;
	displayName: string;
	description: string | null;
	version: string;
	latestVersion: string;
	kind: NgwaKind;
	trustFacet: TrustFacetValue;
	installedItem: NgwaItem | null;
	isUpdate: boolean;
	registryEntry: RegistryEntry;
}

/**
 * Builds the Store tab's catalog by merging registry entries with installed items.
 * Marks items that are already installed, and identifies available updates.
 */
export function buildStoreCatalog(
	installedItems: NgwaItem[],
	registryEntries: RegistryEntry[]
): NgwaStoreEntry[] {
	return registryEntries.map((entry) => {
		const installed =
			installedItems.find(
				(it) =>
					entryMatchesPkgId(entry.name, it.id) ||
					entryMatchesPkgId(entry.name, it.name) ||
					it.name === entry.name
			) ?? null;

		const isUpdate =
			installed !== null &&
			installed.version !== null &&
			semverCompare(installed.version, entry.latest) < 0;

		const trustFacet: TrustFacetValue =
			'signed' in entry && (entry as Record<string, unknown>).signed === false
				? 'unsigned'
				: 'signed';

		const kind: NgwaKind = (entry.kind as NgwaKind) ?? 'app';

		return {
			id: entry.name,
			name: entry.name,
			displayName: entry.name.replace(/^@ikenga\//, ''),
			description: (entry as { description?: string }).description ?? null,
			version: installed?.version ?? entry.latest,
			latestVersion: entry.latest,
			kind,
			trustFacet,
			installedItem: installed,
			isUpdate,
			registryEntry: entry,
		};
	});
}
