// "Desktop app only" — the shared answer for a control whose backend command
// the headless daemon does not serve to a browser session (2026-10-06 gap
// audit). Prefer hiding such a control; use this reason where a menu row is
// kept but disabled so the user learns why.

import { isRemoteWebSession } from '@/lib/tauri-cmd';

export const DESKTOP_ONLY_REASON = 'Desktop app only';

/** `DESKTOP_ONLY_REASON` in a browser session, `false` otherwise — shaped for
 *  a menu resolver's `disabled(id)` callback. */
export function desktopOnlyReason(): string | false {
	return isRemoteWebSession() ? DESKTOP_ONLY_REASON : false;
}
