import { honestRpcError } from '@/lib/transport/unavailable';

const LATER = 'You can retry later from Ngwa → Store.';

/** The webview's own fetch() (the registry index / detail reads) rejects with
 *  a bare TypeError that names no cause. */
const WEBVIEW_FETCH_FAILURE =
	/^(TypeError: )?(Failed to fetch|NetworkError when attempting to fetch resource\.?|Load failed)$/i;

/** Why the offline-engine install failed, in the user's words. The old catch-all
 *  blamed the registry for every failure (a signature check, a full disk, a
 *  daemon that serves no install) — so say the real cause. */
export function offlineInstallErrorMessage(e: unknown): string {
	const raw = e instanceof Error ? e.message : String(e);
	// The registry index is signature-checked before anything is installed: a
	// failed check is a trust problem, not a connectivity one.
	if (/\bsignature\b/i.test(raw) && !/\bintegrity\b/i.test(raw)) {
		return `Couldn't install the offline engine: the registry index couldn't be verified (signature check failed), so nothing was installed. ${LATER}`;
	}
	if (WEBVIEW_FETCH_FAILURE.test(raw.trim())) {
		return `Couldn't install the offline engine: the registry couldn't be reached (network error). ${LATER}`;
	}
	return `Couldn't install the offline engine: ${honestRpcError(e)}. ${LATER}`;
}
