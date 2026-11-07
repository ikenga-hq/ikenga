// The folders this server lets a session open (its fs allowlist, Rust
// `fs_roots`). Shared by the web folder picker and Settings → Storage so both
// decide "can this caller add a folder?" the same way. UI copy only — the
// daemon / broker decide (`server::rpc_fs_roots`).

import type { AccessStatus } from '@/lib/access/client';
import { RPC_REQUIREMENTS } from '@/lib/access/rpc-requirements.gen';

/** Why the caller can't change their own folder list, or `null` if they can.
 *  `status` null (unknown — e.g. the desktop app, or the probe failed) reads
 *  as "can": the server still refuses, and the refusal is shown. */
export function cannotEditRootsReason(status: AccessStatus | null): string | null {
	if (!status) return null;
	if (status.share) return 'You are working in a project someone shared with you.';
	const need = RPC_REQUIREMENTS.fs_roots_add?.caps ?? ['files', 'settings'];
	const have = new Set(status.caps);
	const missing = need.filter((c) => !have.has(c));
	if (missing.length === 0) return null;
	return status.credential.via === 'device'
		? "This device's access level can't change which folders are open."
		: "Your access level can't change which folders are open.";
}

/** Who to ask when the caller can't add a folder themselves. */
export function askForFoldersCopy(status: AccessStatus | null): string {
	return status?.tier === 't1'
		? 'Ask an admin of this server to add a folder for you.'
		: 'Ask the person who runs this server to add a folder for you.';
}

/** An absolute path on the server (POSIX, or a Windows drive path). `.` and
 *  relative paths never are: the daemon would resolve them against its own
 *  working directory. */
export function isAbsoluteServerPath(p: string): boolean {
	return p.startsWith('/') || /^[A-Za-z]:[\\/]/.test(p);
}

/** A folder path the user may type into their list. Always an absolute path;
 *  the desktop app also takes `~` / `~/…`, which Rust (`fs_roots`) expands
 *  against this machine's home. A server would expand `~` against its own
 *  process, not the caller's home, so a browser session gets absolute only. */
export function isAddableFolderPath(p: string, desktop: boolean): boolean {
	if (isAbsoluteServerPath(p)) return true;
	return desktop && (p === '~' || p.startsWith('~/') || p.startsWith('~\\'));
}

/** An example folder for the add field and its error, in this machine's form
 *  on the desktop app (a browser session can't know its server's OS). */
export function folderPathExample(desktop: boolean, windows: boolean): string {
	if (desktop && windows) return 'C:\\Users\\you\\projects';
	return '/home/you/projects';
}

/** Whether an RPC error is a permission refusal (vs. a bad path). */
export function isPermissionError(message: string): boolean {
	return /^(forbidden|requires_t1)\b/.test(message) || message.includes('missing=');
}
