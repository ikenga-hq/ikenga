// One-shot rail → pins seeding (WP-22).
//
// Package rail entries are *not* pins: `usePkgActivityBarEntries`
// (`@/lib/pkg/use-activity-bar-entries`) derives them at runtime from each
// installed manifest's `ui.views[0]` (manifest v5; during the alias window
// the activity-bar registry maps `ui.views[]` onto the legacy NavEntry wire
// shape — WP-28; the `ui.nav` manifest field itself is gone per DEC-37), so a Zustand
// store migration can never see them. For pkgs installed *before* v5 there
// was no `pin_on_install` either — this reconciler covers that cohort.
// Without it, a user with N rail icons today loses all N the moment the
// pinned-rail UI ships, because none of them were ever written to
// `activity_pins`.
//
// This module runs once, after boot, from `src/boot/primary.tsx` — never
// from the Zustand `persist` `migrate` (that must stay synchronous) and
// never from `workspace.tsx` (owned by WP-20 this wave). It is guarded by a
// one-shot KV flag in `settings_kv` so a user's later unpin is never
// resurrected on a subsequent boot.

import {
	activityPinsAdd,
	activityPinsList,
	pkgKernelStatus,
	settingsGetAll,
	settingsSet,
} from '@/lib/tauri-cmd';
import type { ActivityPin } from '@/lib/tauri-cmd';
import type { PkgActivityBarEntry } from '@/lib/pkg/use-activity-bar-entries';

/** Bumped alongside the shell-store migration version this seed pairs with
 *  (WP-02 landed store version 16). A future re-seed (if ever needed) gets
 *  its own `.vNN` flag rather than reusing this one. */
export const SEED_PINS_FLAG = 'shell.pins.seeded.v16';

const LOG_PREFIX = '[seed-pins]';

/** True when `pin` already covers `entry` — i.e. seeding it again would be
 *  a duplicate. Matches on the pair the brief specifies: `kind === 'route'`
 *  with the same `target`, or (belt-and-braces) the same `manifestId` in
 *  the rare case a pkg's rail entry was already pinned as an artifact under
 *  its own manifest id. */
function isAlreadyPinned(entry: PkgActivityBarEntry, pins: readonly ActivityPin[]): boolean {
	return pins.some((p) => {
		if (p.kind === 'route' && p.target === entry.route) return true;
		if (p.manifestId && p.manifestId === entry.pkg_id) return true;
		return false;
	});
}

/** Pure helper (unit-tested in isolation): given the current registry order
 *  and the current pin set, return the registry entries that still need a
 *  pin, in registry order. */
export function computeEntriesToSeed(
	entries: readonly PkgActivityBarEntry[],
	existingPins: readonly ActivityPin[]
): PkgActivityBarEntry[] {
	return entries.filter((e) => !isAlreadyPinned(e, existingPins));
}

/** A pin's label is the pkg's own view label ("Wikipedia"), not the rail
 *  entry's `label`, which the kernel sets to `views[0]`'s pkg display name.
 *  (`entry.nav` is the `ui.views` list mapped onto the NavEntry wire shape —
 *  labels are view titles.) */
export function pinLabelFor(entry: PkgActivityBarEntry): string {
	return entry.nav?.[0]?.label?.trim() || entry.pkg_name?.trim() || entry.label;
}

/** Post-hydration async reconciler. Fire-and-forget from boot: on failure
 *  it logs and returns without throwing, and without setting the seeded
 *  flag, so the next boot gets another chance. */
export async function seedPinsFromRail(): Promise<void> {
	try {
		const all = await settingsGetAll();
		if (all[SEED_PINS_FLAG]) {
			console.info(`${LOG_PREFIX} already seeded`);
			return;
		}

		const [status, existingPins] = await Promise.all([pkgKernelStatus(), activityPinsList()]);
		const registry = (status.registries.activity_bar ?? {}) as {
			entries?: PkgActivityBarEntry[];
		};
		const entries = registry.entries ?? [];

		const toSeed = computeEntriesToSeed(entries, existingPins);

		for (const entry of toSeed) {
			await activityPinsAdd({
				kind: 'route',
				target: entry.route,
				label: pinLabelFor(entry),
				iconLucide: entry.icon ?? null,
				sectionId: null,
				manifestId: null,
			});
		}

		await settingsSet(SEED_PINS_FLAG, JSON.stringify(true));
		console.info(`${LOG_PREFIX} seeded ${toSeed.length} pins`);
	} catch (err) {
		// Never block boot, and never mark the flag set on a partial/failed
		// run — the next boot should retry from scratch.
		console.warn(`${LOG_PREFIX} failed, will retry next boot:`, err);
	}
}
