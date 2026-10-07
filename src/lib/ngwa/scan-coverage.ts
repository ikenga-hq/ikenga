// What the Ngwa config scan did NOT read.
//
// `sources.engine_config` reads `ok: false` with an error starting
// "partially unreadable" when the scan answered but skipped something
// (`server::shared::ngwa::PARTIALLY_UNREADABLE` — keep the two in lockstep):
//
//   partially unreadable — project roots not scanned: `/a` (why); `/b` (why) · unreadable: `/f` (why)
//
// A registered project's root named under "project roots not scanned" was
// not read at all (on the headless daemon: outside the fs allowlist), so its
// Scopes column is unknown — never "nothing here". The rows the scan did read
// are still present and still counted, so a partial scan does not make every
// primitive cell unknown the way a failed scan does.

/** `server::shared::ngwa::PARTIALLY_UNREADABLE`. */
export const PARTIALLY_UNREADABLE = 'partially unreadable';

const ROOTS_HEAD = 'project roots not scanned: ';
const FILES_SEP = ' · unreadable: ';

/** A source error saying the scan read some, not all, of what it was asked to. */
export function isPartiallyUnreadable(error: string | null | undefined): boolean {
	return typeof error === 'string' && error.startsWith(PARTIALLY_UNREADABLE);
}

/** Primitive-source health entries whose loss makes EVERY primitive cell
 *  unknown: failed outright, not merely partial. */
export function fullyDown<T extends { error: string | null }>(sources: readonly T[]): T[] {
	return sources.filter((s) => !isPartiallyUnreadable(s.error));
}

function stripSlashes(p: string): string {
	return p.length > 1 ? p.replace(/[\\/]+$/, '') : p;
}

/**
 * Why the scan did not read the project rooted at `root`, from the
 * `engine_config` error — the message it gave for that root (e.g. "path
 * outside allowlist: /x") — or `null` when that root was read (or the error
 * is not a partial-scan error).
 */
export function rootNotScanned(
	error: string | null | undefined,
	root: string | null | undefined
): string | null {
	if (!root || !isPartiallyUnreadable(error)) return null;
	const text = error as string;
	const at = text.indexOf(ROOTS_HEAD);
	if (at < 0) return null;
	const end = text.indexOf(FILES_SEP, at);
	const section = text.slice(at + ROOTS_HEAD.length, end < 0 ? undefined : end);
	const want = stripSlashes(root);
	const entries = [...section.matchAll(/`([^`]+)` \(/g)];
	for (let i = 0; i < entries.length; i++) {
		const m = entries[i];
		if (!m || stripSlashes(m[1] ?? '') !== want) continue;
		// The message runs to the `)` closing this entry: just before the
		// next "; `<path>` (" entry, or the section's end.
		const from = (m.index ?? 0) + m[0].length;
		const next = entries[i + 1];
		const raw = next ? section.slice(from, next.index) : section.slice(from);
		const msg = raw.replace(/\)(;\s*)?\s*$/, '');
		return msg || 'not scanned';
	}
	return null;
}

/** The Scopes wording for a project column the scan did not read. */
export function notScannedReason(label: string, why: string): string {
	return /outside allowlist/.test(why)
		? `Not scanned: ${label}'s root is outside this server's allowlist, so its state is unknown`
		: `Not scanned: ${label}'s root could not be read (${why}), so its state is unknown`;
}
