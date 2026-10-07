import { honestRpcError } from '@/lib/transport/unavailable';

/** Why the offline-engine install failed, in the user's words. The old catch-all
 *  blamed the registry for every failure (a signature check, a full disk, a
 *  daemon that serves no install) — so say the real cause. */
export function offlineInstallErrorMessage(e: unknown): string {
	return `Couldn't install the offline engine: ${honestRpcError(e)}. You can retry later from Ngwa → Store.`;
}
