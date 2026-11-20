// WP-17: pure helpers for the outside-link UI (box-files). The form's expiry
// and passcode rules mirror the server's (`access::file_links::create_spec`):
// 1..=365 days or never, a passcode of 6..=128 characters. The server decides;
// these only explain a refusal before the round trip.

import type { AccessStatus, FileLinkState, FileLinkView } from '@/lib/access/client';

/** Expiry choices for "Get a link" (D-R1-8): default 7 days, "never" allowed. */
export const LINK_EXPIRY_CHOICES: ReadonlyArray<{ days: number | null; label: string }> = [
	{ days: 1, label: 'In 1 day' },
	{ days: 7, label: 'In 7 days' },
	{ days: 30, label: 'In 30 days' },
	{ days: null, label: 'Never' },
];
export const DEFAULT_LINK_DAYS = 7;

export const PASSCODE_MIN = 6;
export const PASSCODE_MAX = 128;

/** Why this passcode can't be used, or `null` (empty = no passcode). */
export function passcodeProblem(passcode: string): string | null {
	if (passcode === '') return null;
	const n = Array.from(passcode).length;
	if (n < PASSCODE_MIN) return `A passcode needs at least ${PASSCODE_MIN} characters`;
	if (n > PASSCODE_MAX) return `A passcode can be at most ${PASSCODE_MAX} characters`;
	return null;
}

/** The create arm's expiry args for a choice (`null` days = never). */
export function expiryArgs(days: number | null): { expiresInDays: number } | { never: true } {
	return days === null ? { never: true } : { expiresInDays: days };
}

/** The create arm's args for the form's state; an empty passcode is omitted. */
export function createArgs(f: { path: string; days: number | null; passcode: string }) {
	return {
		path: f.path,
		...expiryArgs(f.days),
		...(f.passcode === '' ? {} : { passcode: f.passcode }),
	};
}

/** Why "Create link" is disabled, or `null`. */
export function linkBlocker(f: {
	path: string;
	days: number | null;
	passcode: string;
}): string | null {
	if (!f.path.trim()) return 'Choose a file first';
	if (f.days !== null && !(Number.isInteger(f.days) && f.days >= 1 && f.days <= 365)) {
		return 'Choose when the link expires';
	}
	return passcodeProblem(f.passcode);
}

/** Whether to offer "Get a link" at all: only when the server turned it on and this is not a share. */
export function linksOffered(
	status: Pick<AccessStatus, 'fileLinksEnabled' | 'share'> | null | undefined
): boolean {
	return Boolean(status?.fileLinksEnabled) && !status?.share;
}

/** Whether the caller may use the admin "All links" view (the server decides). */
export function isLinksAdmin(
	status: Pick<AccessStatus, 'principal' | 'adminStrength'> | null | undefined
): boolean {
	return Boolean(status?.principal.isAdmin && status.adminStrength);
}

export const LINK_STATE_LABEL: Readonly<Record<FileLinkState, string>> = {
	live: 'Live',
	expired: 'Expired',
	revoked: 'Revoked',
	locked: 'Locked',
};

/** "Never", "in 6 d", "in 3 h", or "ended". */
export function expiryText(expiresAt: number | null, now: number): string {
	if (expiresAt === null) return 'Never';
	const ms = expiresAt - now;
	if (ms <= 0) return 'Ended';
	const h = Math.ceil(ms / 3_600_000);
	if (h < 48) return `In ${h} h`;
	return `In ${Math.ceil(h / 24)} d`;
}

/** Only a live or locked link can still be revoked. */
export function canRevoke(link: Pick<FileLinkView, 'state' | 'revokedAt'>): boolean {
	return link.revokedAt === null && (link.state === 'live' || link.state === 'locked');
}

/** Admin filter by account: the rows of one owner (`null` = everyone). */
export function filterByOwner<T extends { ownerPrincipalId: string }>(
	links: readonly T[],
	owner: string | null
): T[] {
	return owner === null ? [...links] : links.filter((l) => l.ownerPrincipalId === owner);
}

/** Distinct owners of the rows, for the filter menu, by username. */
export function ownersOf(
	links: readonly { ownerPrincipalId: string; ownerUsername: string | null }[]
): { id: string; name: string }[] {
	const seen = new Map<string, string>();
	for (const l of links)
		if (!seen.has(l.ownerPrincipalId))
			seen.set(l.ownerPrincipalId, l.ownerUsername ?? l.ownerPrincipalId.slice(0, 8));
	return [...seen].map(([id, name]) => ({ id, name })).sort((a, b) => a.name.localeCompare(b.name));
}

/** Join a project root and a project-relative path into the absolute path the arm wants. */
export function absoluteFilePath(root: string | null | undefined, relative: string): string | null {
	if (!root) return null;
	const r = root.replace(/\/+$/, '');
	const p = relative.replace(/^\/+/, '');
	return r && p ? `${r}/${p}` : null;
}
