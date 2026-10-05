// Catalog visibility for registry index entries.
//
// The signed index marks some pkgs `visibility: "hidden"`. A hidden pkg stays
// installable by exact name (installed rows, update detection, health
// reinstall, the engine fallback) but is kept out of browse and search.
//
// `@ikenga/registry-client` parses the index with a schema that predates the
// field, and Zod strips unknown keys, so the parsed entries never carry it.
// The signature-verified raw bytes ARE returned alongside the parsed index, so
// `withVerifiedVisibility` copies the flag back from them. Nothing here reads
// bytes that were not already verified by the library.

import type { FetchedIndex, RegistryEntry } from '@ikenga/registry-client';

/** True when the signed index holds this entry out of browse and search. */
export function isHiddenRegistryEntry(entry: RegistryEntry): boolean {
	return (entry as { visibility?: string }).visibility === 'hidden';
}

/**
 * Re-attach `visibility: "hidden"` to the parsed entries it was stripped from.
 * Fails open: if the raw bytes cannot be read, the index is returned as is, so
 * a problem here can show an extra pkg but never hide a pkg that should show.
 */
export function withVerifiedVisibility(fetched: FetchedIndex): FetchedIndex {
	let hidden: Set<string>;
	try {
		const json = JSON.parse(new TextDecoder().decode(fetched.raw)) as {
			pkgs?: Array<{ name?: unknown; visibility?: unknown }>;
		};
		hidden = new Set(
			(json.pkgs ?? [])
				.filter((p) => p?.visibility === 'hidden' && typeof p.name === 'string')
				.map((p) => p.name as string)
		);
	} catch {
		return fetched;
	}
	if (hidden.size === 0) return fetched;
	return {
		...fetched,
		index: {
			...fetched.index,
			pkgs: fetched.index.pkgs.map((e) =>
				hidden.has(e.name) ? { ...e, visibility: 'hidden' as const } : e
			),
		},
	};
}
