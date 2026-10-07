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

/** Whether an RPC error is a permission refusal (vs. a bad path). */
export function isPermissionError(message: string): boolean {
	return /^(forbidden|requires_t1)\b/.test(message) || message.includes('missing=');
}
