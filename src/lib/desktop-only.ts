// "Desktop app only" — the shared answer for a control whose backend command
// the headless daemon does not serve to a browser session (2026-10-06 gap
// audit). Prefer hiding such a control; use this reason where a menu row is
// kept but disabled so the user learns why.

import { isRemoteWebSession } from '@/lib/tauri-cmd';
import { NOT_AVAILABLE_ON_SERVER } from '@/lib/transport/unavailable';

export { NOT_AVAILABLE_ON_SERVER };

export const DESKTOP_ONLY_REASON = 'Desktop app only';

/** `DESKTOP_ONLY_REASON` in a browser session, `false` otherwise — shaped for
 *  a menu resolver's `disabled(id)` callback. */
export function desktopOnlyReason(): string | false {
	return isRemoteWebSession() ? DESKTOP_ONLY_REASON : false;
}

/** Package install / update are not served by the daemon yet (gap audit rank 3:
 *  `pkg_install_*`, `oba_install_*`, `oba_update` wait on WP-18b executor
 *  routing). In a browser session the install and update buttons are disabled
 *  and read this instead of failing with a raw error. Delete this gate (and its
 *  call sites) when those commands are served. */
export function installUnavailableReason(): string | false {
	return isRemoteWebSession() ? NOT_AVAILABLE_ON_SERVER : false;
}
