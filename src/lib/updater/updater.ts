// Thin typed wrapper over @tauri-apps/plugin-updater. The plugin's check()
// hits the endpoint declared in tauri.conf.json plugins.updater.endpoints
// (the GitHub Releases latest.json), verifies the bundle sig against the
// embedded minisign pubkey, and exposes downloadAndInstall() + relaunch().
//
// This file is the only allowed consumer of the update surface — it routes
// through the transport shim so that browser sessions do not statically depend
// on the Tauri plugin packages.

import {
	checkForUpdate as transportCheckForUpdate,
	relaunchApp,
	type UpdateInfo,
} from '@/lib/transport';

export type { UpdateInfo } from '@/lib/transport';

/**
 * Check the configured endpoint for a newer release. Returns null when the
 * current version is up to date, or when the check fails (network down,
 * endpoint 404, sig mismatch — log + degrade gracefully).
 */
export async function checkForUpdate(): Promise<UpdateInfo | null> {
	return transportCheckForUpdate();
}

/**
 * Download + install the update bundle. Does NOT relaunch — see `restartApp`.
 *
 * The relaunch is deliberately a separate step. On Linux especially, the
 * install applies through an elevated `pkexec`/`dpkg` step whose progress the
 * download callback can't see, so the byte-count bar freezes near the end;
 * if we then relaunch immediately the window tears down mid-flow and the whole
 * thing reads as a crash — even though the install actually succeeded. Letting
 * the UI hold at an explicit "installed — restart to finish" state before
 * calling `restartApp()` keeps the restart a deliberate, visible act.
 *
 * `onProgress` reports total bytes downloaded so the UI can render a bar.
 * Tauri reports `started` / `progress` / `finished` events; we collapse
 * them to a running byte count.
 */
export async function installUpdate(
	info: UpdateInfo,
	onProgress?: (bytesDownloaded: number, totalBytes: number | null) => void
): Promise<void> {
	if (!info.handle?.downloadAndInstall) {
		throw new Error('[updater] installUpdate called without a valid desktop update handle');
	}

	let downloaded = 0;
	let total: number | null = null;
	await info.handle.downloadAndInstall((event: any) => {
		switch (event.event) {
			case 'Started':
				total = event.data.contentLength ?? null;
				downloaded = 0;
				break;
			case 'Progress':
				downloaded += event.data.chunkLength;
				onProgress?.(downloaded, total);
				break;
			case 'Finished':
				onProgress?.(total ?? downloaded, total);
				break;
		}
	});
}

/** Relaunch to complete an already-installed update. Kept separate from
 * `installUpdate` so it is always gated behind a user click — including on the
 * opt-in auto-install path, which installs in the background but still waits
 * for the user to press Restart. */
export async function restartApp(): Promise<void> {
	await relaunchApp();
}
