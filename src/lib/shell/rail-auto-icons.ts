// Automatic rail icons for app pkgs (R-3).
//
// Every installed + enabled pkg that contributes a view gets one rail icon with
// no pinning step. The icon is derived from the live kernel snapshot, so
// disabling or uninstalling a pkg removes it with no cleanup. Two things keep it
// polite: a pkg the user already has a pin into (including a kernel
// `pin_on_install` pin) gets no second icon, and the user can hide an icon.
//
// "Hidden" is a list of pkg ids kept in `settings_kv` (key below), the same
// durable per-user store the shell settings use. The activity pins table has no
// hidden state and changing it would need Rust, so the list lives beside it.

import { create } from 'zustand';
import type { PkgActivityBarEntry } from '@/lib/pkg/use-activity-bar-entries';
import { type Pin, pinsOfPkg } from '@/lib/shell/pins-store';
import { settingsGet, settingsSet } from '@/lib/tauri-cmd';

export const RAIL_HIDDEN_KV = 'rail.hiddenAutoPkgs';

/** The entries that earn an automatic icon: enabled, not hidden by the user and
 *  not already represented by a pin. One per pkg (entries are one per pkg;
 *  duplicates are dropped defensively). `enabled` null = snapshot unknown. */
export function autoRailEntries(
	entries: readonly PkgActivityBarEntry[],
	enabled: ReadonlySet<string> | null,
	pins: readonly Pin[],
	hidden: ReadonlySet<string>
): PkgActivityBarEntry[] {
	const seen = new Set<string>();
	const out: PkgActivityBarEntry[] = [];
	for (const e of entries) {
		if (seen.has(e.pkg_id)) continue;
		if (!enabled || !enabled.has(e.pkg_id)) continue;
		if (hidden.has(e.pkg_id)) continue;
		if (pinsOfPkg(pins, e.pkg_id).length > 0) continue;
		seen.add(e.pkg_id);
		out.push(e);
	}
	return out;
}

interface RailAutoState {
	hidden: ReadonlySet<string>;
	hydrated: boolean;
	hydrate: () => Promise<void>;
	hide: (pkgId: string) => Promise<void>;
	show: (pkgId: string) => Promise<void>;
}

async function persist(ids: ReadonlySet<string>): Promise<void> {
	try {
		await settingsSet(RAIL_HIDDEN_KV, JSON.stringify([...ids].sort()));
	} catch (err) {
		console.warn('[rail] saving hidden app icons failed:', err);
	}
}

export const useRailAutoStore = create<RailAutoState>((set, get) => ({
	hidden: new Set<string>(),
	hydrated: false,
	hydrate: async () => {
		if (get().hydrated) return;
		try {
			const raw = await settingsGet(RAIL_HIDDEN_KV);
			const parsed: unknown = raw ? JSON.parse(raw) : [];
			const ids = Array.isArray(parsed)
				? parsed.filter((x): x is string => typeof x === 'string')
				: [];
			// Keep anything hidden while the read was in flight.
			set({ hidden: new Set([...ids, ...get().hidden]), hydrated: true });
		} catch {
			set({ hydrated: true });
		}
	},
	hide: async (pkgId) => {
		const next = new Set(get().hidden);
		next.add(pkgId);
		set({ hidden: next });
		await persist(next);
	},
	show: async (pkgId) => {
		const next = new Set(get().hidden);
		next.delete(pkgId);
		set({ hidden: next });
		await persist(next);
	},
}));
