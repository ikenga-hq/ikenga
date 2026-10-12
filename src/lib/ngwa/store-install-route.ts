// Where a registry row installs FROM, per host (Ngwa Store).
//
// On the desktop a registry row is a PACKAGE: the pkg kernel installs it. A
// browser session has no pkg kernel (the pkg set is the server operator's), so
// almost every registry row used to read "Packages are installed by the server
// operator" — including skills, which are not pkgs in any way that matters:
// they are Claude-config primitives the daemon DOES install, into the signed-in
// account's own vault, via the Ọba handlers (`oba_install_*`).
//
// A registry row that is a SKILL or BUNDLE and that the signed primitive catalog
// also lists (Q2: `alsoFrom` / `alsoEntry`, "also: npx") therefore installs, in
// a browser session, as that catalog entry — the same `installPrimitive` call a
// catalog row makes, at the catalog's pin. Everything else (apps, engines,
// tools, a skill pkg that ships a sidecar / MCP / UI, a skill pkg with no
// catalog twin) stays blocked with the operator reason.
//
// This only decides; it never fetches. Pure, so the row, the sheet and the
// surface's install handler all read the same answer.

import type { NgwaCatalogRow, NgwaStoreEntry } from '@/lib/ngwa/enrichment';
import { consentGroups, type StoreManifest } from '@/lib/ngwa/store-detail';
import { catalogPin, isPinned } from '@/lib/registry/primitives';
import type { ClaudeStoreEntry } from '@/lib/tauri-cmd';
import { NOT_AVAILABLE_ON_SERVER_YET } from '@/lib/transport/unavailable';

export type StoreInstallRoute =
	/** The pkg kernel installs it (the desktop). */
	| { via: 'pkg' }
	/** The Ọba path installs the catalog twin into the account's vault. */
	| {
			via: 'oba';
			/** The catalog row to hand `installPrimitive`. */
			row: NgwaCatalogRow;
			/** The catalog pins the entry (a SHA and/or a content hash). */
			pinned: boolean;
			/** The sentence the sheet and the row tooltip read. */
			note: string;
	  }
	/** Cannot be installed here; `reason` is the sentence shown. */
	| { via: 'blocked'; reason: string };

/** What this host can do. `pkg` = a pkg install handler exists; `oba` = the Ọba
 *  primitive install handler exists; `reason` = why a pkg cannot be installed
 *  (shown on every blocked row). */
export interface StoreRouteCaps {
	pkg: boolean;
	oba: boolean;
	reason?: string;
}

/** The manifest blocks that make a registry pkg more than a skill. Empty = it is
 *  skill-only (declares intent, grants itself nothing, runs nothing). */
export function nonSkillBlocks(manifest: StoreManifest): string[] {
	const m = manifest as unknown as Record<string, unknown>;
	const nonEmpty = (v: unknown) => (Array.isArray(v) ? v.length > 0 : Boolean(v));
	const out: string[] = [];
	if (nonEmpty(m.mcp)) out.push('mcp');
	if (nonEmpty(m.sidecars)) out.push('sidecars');
	if (nonEmpty(m.engine)) out.push('engine');
	if (nonEmpty(m.cron)) out.push('cron');
	if (nonEmpty(m.workflows)) out.push('workflows');
	if (nonEmpty(m.window)) out.push('window');
	if (nonEmpty(m.iyke)) out.push('iyke');
	if (nonEmpty(m.migrations)) out.push('migrations');
	const ui = (m.ui ?? {}) as Record<string, unknown>;
	for (const k of [
		'routes',
		'views',
		'companion_panels',
		'widgets',
		'explorer_sections',
		'context_actions',
		'command_palette',
	]) {
		if (nonEmpty(ui[k])) {
			out.push('ui');
			break;
		}
	}
	// A skill asks for nothing: any permission group means it is not one.
	if (consentGroups(manifest).length > 0) out.push('permissions');
	return out;
}

/** The catalog twin of a registry row as a catalog row, or null when the row has
 *  none or is not a skill / bundle. `vault` fills `installed`. */
export function altCatalogRow(
	entry: NgwaStoreEntry,
	vault: readonly ClaudeStoreEntry[]
): NgwaCatalogRow | null {
	const e = entry.alsoEntry;
	if (!e) return null;
	if (entry.kind !== 'skill' && entry.kind !== 'bundle') return null;
	if (e.kind !== entry.kind) return null;
	return {
		id: `cat:${e.kind}:${e.name}`,
		name: e.name,
		kind: e.kind,
		storeKind: e.kind,
		source: e.source,
		url: e.url,
		version: e.version,
		description: e.description ?? entry.description,
		publisher: e.publisher ?? null,
		entry: e,
		installed: vault.find((v) => v.kind === e.kind && v.name === e.name) ?? null,
		isUpdate: false,
	};
}

/** The sentence for an Ọba-routed install. */
export function obaRouteNote(entry: NgwaStoreEntry, source: 'git' | 'npx'): string {
	return `Installs the ${entry.kind} into your account (${source} source)`;
}

/**
 * How `entry` installs on this host. `manifest` is the registry detail's
 * manifest when it has been read (null = not read yet: the answer is then the
 * optimistic one, and the sheet re-asks once the manifest is in, so a pkg that
 * turns out to ship a sidecar / MCP / UI is blocked before any consent).
 */
export function storeInstallRoute(
	entry: NgwaStoreEntry,
	caps: StoreRouteCaps,
	manifest: StoreManifest | null,
	vault: readonly ClaudeStoreEntry[]
): StoreInstallRoute {
	if (caps.pkg) return { via: 'pkg' };
	const blocked: StoreInstallRoute = {
		via: 'blocked',
		reason: caps.reason ?? NOT_AVAILABLE_ON_SERVER_YET,
	};
	// A pkg on disk that failed to load is a pkg problem, not a skill to re-fetch.
	if (!caps.oba || entry.broken) return blocked;
	const row = altCatalogRow(entry, vault);
	if (!row) return blocked;
	if (manifest && nonSkillBlocks(manifest).length > 0) return blocked;
	return {
		via: 'oba',
		row,
		pinned: isPinned(row.entry),
		note: obaRouteNote(entry, row.source),
	};
}

/** The pin the Ọba route installs against (the catalog's), for display. */
export function routePin(route: StoreInstallRoute) {
	return route.via === 'oba' ? catalogPin(route.row.entry) : null;
}
