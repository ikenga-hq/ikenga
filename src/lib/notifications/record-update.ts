// WP-40 `update` producers for the two update checks that live in the webview:
//
//   * the app updater — `checkForUpdate()` in `src/lib/updater/updater.ts`
//     (tauri-plugin-updater's JS `check()`, boot + every 6 h);
//   * the pkg registry cross-reference — `usePkgsDerived().updates`, recorded
//     from `src/shell/pkg-auto-updater.tsx` (mounted once in the workspace).
//
// Both only hand facts to Rust (`notifications_record_update`); the copy,
// action and the once-per-version dedupe key are built there, so re-checking
// every 6 h never re-announces a version. Fire-and-forget: a failed write
// must never break an update check, so these never reject — errors are
// logged and swallowed.

import { isTauri, notificationsRecordUpdate } from '@/lib/tauri-cmd';

export interface PkgUpdateFact {
	id: string;
	name?: string | null;
	/** The newer version the registry offers. */
	latest: string | null;
}

/** Record "Ikenga <version> is available". No-op outside the desktop runtime. */
export async function recordShellUpdateAvailable(version: string | null | undefined): Promise<void> {
	try {
		if (!version || !isTauri()) return;
		await notificationsRecordUpdate({ source: 'shell', version });
	} catch (e) {
		console.warn('[notifications] could not record shell update', e);
	}
}

/**
 * Record one row per outdated pkg (`<name> <latest> is available`). Rows
 * already announced for that version are dropped in Rust.
 */
export async function recordPkgUpdatesAvailable(updates: readonly PkgUpdateFact[]): Promise<void> {
	try {
		if (!updates.length || !isTauri()) return;
	} catch (e) {
		console.warn('[notifications] could not record pkg updates', e);
		return;
	}
	for (const u of updates) {
		if (!u.latest) continue;
		try {
			await notificationsRecordUpdate({
				source: 'pkg',
				version: u.latest,
				pkgId: u.id,
				pkgName: u.name ?? null,
			});
		} catch (e) {
			console.warn(`[notifications] could not record update for ${u.id}`, e);
		}
	}
}
