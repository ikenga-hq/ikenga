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
import {
	catalogPin,
	catalogPinMoved,
	isPinned,
	type PrimitiveCatalogEntry,
} from '@/lib/registry/primitives';
import type { ClaudeStoreEntry, ClaudeStoreKind } from '@/lib/tauri-cmd';

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
export function enrichNgwaItems(items: NgwaItem[], registryEntries: RegistryEntry[]): NgwaItem[] {
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

const NGWA_KIND_SET: ReadonlySet<string> = new Set<NgwaKind>([
	'app',
	'engine',
	'tool',
	'sidecar',
	'skill',
	'agent',
	'command',
	'hook',
	'bundle',
	'schedule',
	'workflow',
]);

/**
 * The Ngwa kind for a Store row.
 *
 * The registry index's `kind` is the manifest's free-form hint ("skill" |
 * "embedded" | "windowed" | "engine" | "app" | "bundle"), not an Ngwa kind, so
 * it can't be cast. An installed pkg uses the kernel's own classification
 * (`pkg_kind` in `commands/ngwa.rs`: engine → app (ui) → tool (mcp) →
 * sidecar), the same one the Installed tab shows. Otherwise the hint is
 * mapped, and the `@ikenga/mcp-*` MCP servers (`ikenga-pkgs/packages/mcp/`),
 * which declare the hint "skill", are tools.
 */
export function storeKindFor(entry: RegistryEntry, installed: NgwaItem | null): NgwaKind {
	if (installed && NGWA_KIND_SET.has(installed.kind)) return installed.kind as NgwaKind;
	const hint = (entry.kind ?? '').toLowerCase();
	switch (hint) {
		case 'engine':
			return 'engine';
		case 'bundle':
			return 'bundle';
		case 'app':
		case 'embedded':
		case 'windowed':
			return 'app';
		case 'skill':
			return /^@ikenga\/mcp-/.test(entry.name) ? 'tool' : 'skill';
		default:
			return NGWA_KIND_SET.has(hint) ? (hint as NgwaKind) : 'app';
	}
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
	/** R57 · Q2: the signed catalog lists the same kind+name, so its row is
	 *  folded into this one; the l3 line notes `also: <source>`. */
	alsoFrom?: { source: 'git' | 'npx'; url: string } | null;
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

		const kind = storeKindFor(entry, installed);

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

// ─── R57 · catalog rows (git / npx primitives from the signed catalog) ──────

/** The Store facet a row installs FROM (R57 Source chips). */
export type StoreSource = 'registry' | 'git' | 'npx';

/** A row's Kind facet value: an Ngwa kind, or `mcp` (a catalog MCP entry has
 *  no Ngwa kind of its own). */
export type StoreRowKind = NgwaKind | 'mcp';

/** One signed-catalog primitive as a Store row (R57 flow 1). */
export interface NgwaCatalogRow {
	/** `cat:<kind>:<name>` — never collides with a registry pkg name. */
	id: string;
	name: string;
	kind: StoreRowKind;
	storeKind: ClaudeStoreKind;
	source: 'git' | 'npx';
	url: string;
	version: string;
	description: string | null;
	publisher: string | null;
	entry: PrimitiveCatalogEntry;
	/** The vault record when installed (any provenance), else null. */
	installed: ClaudeStoreEntry | null;
	/** Q4: a catalog install whose catalog pin has moved (pure comparison). */
	isUpdate: boolean;
}

/** The Ngwa-facing kind for a catalog primitive kind. */
function catalogRowKind(kind: ClaudeStoreKind): StoreRowKind {
	return kind === 'mcp' ? 'mcp' : NGWA_KIND_SET.has(kind) ? (kind as NgwaKind) : 'skill';
}

/**
 * R57 · Q2 — does a registry row stand for the same primitive as a catalog
 * entry? The matching rule:
 *   1. the registry row's Store kind (`storeKindFor`, e.g. `skill`) equals the
 *      catalog entry's kind — kinds never cross-match; and
 *   2. the registry name, with its npm scope (`@ikenga/`) and then a leading
 *      `<kind>-` or `pkg-` prefix stripped, equals the catalog name
 *      (case-insensitive). `@ikenga/skill-groundwork` ↔ skill `groundwork`.
 * A catalog entry matches at most one registry row (the first).
 */
export function registryMatchesCatalog(
	registryName: string,
	registryKind: NgwaKind,
	entry: PrimitiveCatalogEntry
): boolean {
	if (registryKind !== entry.kind) return false;
	const bare = registryName.replace(/^@[^/]+\//, '').toLowerCase();
	const stripped = bare.replace(new RegExp(`^(${registryKind}|pkg)-`), '');
	const want = entry.name.toLowerCase();
	return stripped === want || bare === want;
}

/**
 * Build the Store's catalog rows and fold duplicates into registry rows (Q2).
 * Returns the registry entries (with `alsoFrom` set where a catalog entry
 * folded in) and the remaining catalog rows, which list after them.
 */
export function mergeCatalogIntoStore(
	registryRows: NgwaStoreEntry[],
	catalog: readonly PrimitiveCatalogEntry[],
	vault: readonly ClaudeStoreEntry[]
): { registry: NgwaStoreEntry[]; primitives: NgwaCatalogRow[] } {
	const registry = registryRows.map((r) => ({ ...r }));
	const primitives: NgwaCatalogRow[] = [];
	for (const entry of catalog) {
		const twin = registry.find((r) => !r.alsoFrom && registryMatchesCatalog(r.name, r.kind, entry));
		if (twin) {
			twin.alsoFrom = { source: entry.source, url: entry.url };
			continue;
		}
		const installed = vault.find((v) => v.kind === entry.kind && v.name === entry.name) ?? null;
		const isUpdate =
			installed !== null &&
			installed.fromCatalog === true &&
			isPinned(entry) &&
			entry.kind !== 'hook' &&
			entry.kind !== 'mcp' &&
			catalogPinMoved(installed, entry);
		primitives.push({
			id: `cat:${entry.kind}:${entry.name}`,
			name: entry.name,
			kind: catalogRowKind(entry.kind),
			storeKind: entry.kind,
			source: entry.source,
			url: entry.url,
			version: entry.version,
			description: entry.description,
			publisher: entry.publisher ?? null,
			entry,
			installed,
			isUpdate,
		});
	}
	return { registry, primitives };
}

/** The pin a catalog row updates to, for the Updates strip (Q4). */
export function catalogRowPin(row: NgwaCatalogRow) {
	return catalogPin(row.entry);
}
