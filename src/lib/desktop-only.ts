// "Desktop app only" — the shared answer for a control whose backend command
// the headless daemon does not serve to a browser session (2026-10-06 gap
// audit). Prefer hiding such a control; use this reason where a menu row is
// kept but disabled so the user learns why.

import { isRemoteWebSession } from '@/lib/tauri-cmd';
import { NOT_AVAILABLE_ON_SERVER_YET } from '@/lib/transport/unavailable';

export { NOT_AVAILABLE_ON_SERVER_YET };

export const DESKTOP_ONLY_REASON = 'Desktop app only';

/** `DESKTOP_ONLY_REASON` in a browser session, `false` otherwise — shaped for
 *  a menu resolver's `disabled(id)` callback. */
export function desktopOnlyReason(): string | false {
	return isRemoteWebSession() ? DESKTOP_ONLY_REASON : false;
}

/** Package install / update are not served by the daemon yet (gap audit rank 3:
 *  `pkg_install_*` need a pkg kernel the headless daemon does not run). In a
 *  browser session the pkg install and update buttons are disabled and read
 *  this instead of failing with a raw error. Ọba primitives (skills, agents,
 *  commands, hooks, MCP) are a different matter — see
 *  {@link pkgInstallUnavailableReason} and {@link remoteSourceBlock}. */
export function installUnavailableReason(): string | false {
	return isRemoteWebSession() ? NOT_AVAILABLE_ON_SERVER_YET : false;
}

/** Why a registry **package** cannot be installed from a browser session: the
 *  pkg set is the server operator's (OD-10), installed and trusted on the host,
 *  never per account. */
export const PACKAGES_OPERATOR_REASON = 'Packages are installed by the server operator';

/** `PACKAGES_OPERATOR_REASON` in a browser session, `false` on the desktop —
 *  shaped like {@link installUnavailableReason}. Ngwa's Store uses it for its
 *  registry-pkg rows; its Ọba primitive installs (git / npx fetches into the
 *  signed-in account's own vault) are served and need no gate. */
export function pkgInstallUnavailableReason(): string | false {
	return isRemoteWebSession() ? PACKAGES_OPERATOR_REASON : false;
}

/** Why a path on the user's own disk cannot be installed from a browser
 *  session: the server never reads a path it was handed. */
export const LOCAL_INSTALL_DESKTOP_ONLY_REASON = 'Local installs are desktop-only';

/** Why a git source other than public https cannot be fetched by the server. */
export const REMOTE_SOURCE_HTTPS_REASON =
	'A server install fetches from a public https:// git URL or an owner/repo spec';

/** True for a pasted source that names the user's own disk: an absolute,
 *  home-relative or relative path, a Windows drive path, or `file:`. */
export function isLocalSource(raw: string): boolean {
	const s = raw.trim().replace(/^npx\s+skills\s+add\s+/i, '');
	return /^(\/|~|\.{1,2}(\/|\\|$)|[A-Za-z]:[\\/]|file:)/i.test(s);
}

/** What the server would refuse about a pasted install source, as the sentence
 *  the UI shows instead of letting the click fail — `null` when it would take
 *  it. Mirrors `claude_store::remote` on the daemon, which is the enforcement;
 *  this only saves the round trip. Applies in a browser session only: pass
 *  `isRemoteWebSession()`. */
export function remoteSourceBlock(raw: string, remote: boolean): string | null {
	if (!remote) return null;
	const s = raw.trim().replace(/^npx\s+skills\s+add\s+/i, '');
	if (!s) return null;
	if (isLocalSource(s)) return LOCAL_INSTALL_DESKTOP_ONLY_REASON;
	if (/^(git@|ssh:|git:|http:|ext:|fd:)/i.test(s) || /^-/.test(s))
		return REMOTE_SOURCE_HTTPS_REASON;
	return null;
}

/** {@link remoteSourceBlock} for the current session: `null` on the desktop. */
export function installSourceBlock(raw: string): string | null {
	return remoteSourceBlock(raw, isRemoteWebSession());
}
