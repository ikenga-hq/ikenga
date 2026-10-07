// Turn a raw install failure (the Rust error chain, often with npm's whole
// stderr in it) into a short message with a next step, plus a cleaned-up log
// for "Show details".
//
// The raw text is unbounded: a full disk makes npm print the same
// `TAR_ENTRY_ERROR ENOSPC` warning once per file it failed to write, dozens of
// times. The summary never shows that; the details view shows each distinct
// line once, with a repeat count, truncated.

import { NOT_AVAILABLE_ON_SERVER } from '@/lib/transport/unavailable';

export type InstallErrorKind =
	| 'disk-space'
	| 'network'
	| 'not-found'
	| 'permission'
	| 'integrity'
	| 'cancelled'
	| 'unavailable'
	| 'unknown';

export interface ClassifiedInstallError {
	kind: InstallErrorKind;
	/** One or two sentences: what happened and what to do next. */
	message: string;
	/** The raw output, deduplicated and truncated, for "Show details". */
	details: string;
	/** npm's debug log, when it printed one ("A complete log of this run…"). */
	debugLogPath: string | null;
	/** True when Retry is worth offering. */
	retryable: boolean;
}

/** Ordered: the first rule that matches wins. Disk space is checked before
 *  network and permission because a full disk also surfaces as write errors. */
const RULES: Array<[InstallErrorKind, RegExp]> = [
	['cancelled', /\binstall cancelled\b/i],
	// The browser's daemon doesn't serve install yet (gap audit rank 3).
	['unavailable', /not implemented in headless daemon/i],
	['disk-space', /\bENOSPC\b|no space left on device|not enough space on the disk|os error 112\b/i],
	['integrity', /\bEINTEGRITY\b|integrity mismatch|integrity checksum failed|sha-?512.*mismatch/i],
	['not-found', /\bE404\b|\b404 Not Found\b|status client error \(404|registry returned 404/i],
	[
		'network',
		/\b(EAI_AGAIN|ECONNRESET|ETIMEDOUT|ECONNREFUSED|ENOTFOUND|ENETUNREACH|EHOSTUNREACH)\b|error sending request|tarball stream read|network (is )?unreachable|timed out|dns error/i,
	],
	['permission', /\b(EPERM|EACCES)\b|permission denied|access is denied|os error 5\b|operation not permitted/i],
];

const MAX_DETAIL_LINES = 40;
const MAX_DETAIL_CHARS = 6000;

export function classifyInstallKind(raw: string): InstallErrorKind {
	for (const [kind, re] of RULES) if (re.test(raw)) return kind;
	return 'unknown';
}

export function installErrorMessage(kind: InstallErrorKind, name: string): string {
	switch (kind) {
		case 'disk-space':
			return `Not enough disk space to install ${name}. Free some space and try again.`;
		case 'network':
			return `Couldn't reach the package registry while installing ${name}. Check your connection and try again.`;
		case 'not-found':
			return `${name} wasn't found in the registry. It may have been renamed or unpublished. Refresh the Store and try again.`;
		case 'permission':
			return `Ikenga wasn't allowed to write ${name}'s files. Close anything using its folder (an editor, terminal or antivirus scan) and try again.`;
		case 'integrity':
			return `${name}'s download didn't match its published checksum, so nothing was installed. Try again; if it keeps failing, the published package is broken.`;
		case 'cancelled':
			return `Install of ${name} was cancelled. Nothing was left behind.`;
		case 'unavailable':
			return `${NOT_AVAILABLE_ON_SERVER}. ${name} can be installed from the Ikenga desktop app.`;
		default:
			return `${name} couldn't be installed. Show details has the full error; try again once it's fixed.`;
	}
}

/** npm prints "A complete log of this run can be found in: <path>". */
export function extractDebugLogPath(raw: string): string | null {
	const m = raw.match(/complete log of this run can be found in:?\s*(.+?\.log)\b/i);
	return m ? m[1].trim() : null;
}

/** A line with its variable parts (quoted paths, numbers in paths) blanked,
 *  so the same warning about different files counts as one line. */
function dedupeKey(line: string): string {
	return line
		.replace(/'[^']*'|"[^"]*"/g, "'…'")
		.replace(/[A-Za-z]:\\[^\s,]+|\/[^\s,]+\/[^\s,]+/g, '…')
		.trim();
}

/**
 * The raw log, cleaned for display: blank lines dropped, each distinct line
 * kept once in first-seen order with a `(×N)` repeat count, then capped at
 * {@link MAX_DETAIL_LINES} lines / {@link MAX_DETAIL_CHARS} characters with a
 * note saying how much was cut.
 */
export function dedupeInstallLog(raw: string): string {
	const order: string[] = [];
	const seen = new Map<string, { line: string; count: number }>();
	for (const piece of raw.split(/\r?\n/)) {
		const line = piece.trimEnd();
		if (!line.trim()) continue;
		const key = dedupeKey(line);
		const hit = seen.get(key);
		if (hit) hit.count += 1;
		else {
			seen.set(key, { line, count: 1 });
			order.push(key);
		}
	}
	const lines = order.map((k) => {
		const { line, count } = seen.get(k)!;
		return count > 1 ? `${line}  (×${count})` : line;
	});
	let out = lines.slice(0, MAX_DETAIL_LINES).join('\n');
	let cut = Math.max(0, lines.length - MAX_DETAIL_LINES);
	if (out.length > MAX_DETAIL_CHARS) {
		out = out.slice(0, MAX_DETAIL_CHARS);
		cut = Math.max(cut, 1);
	}
	return cut > 0 ? `${out}\n… truncated (${cut} more line${cut === 1 ? '' : 's'})` : out;
}

export function errorText(e: unknown): string {
	if (e instanceof Error) return e.message;
	if (typeof e === 'string') return e;
	try {
		return JSON.stringify(e);
	} catch {
		return String(e);
	}
}

export function classifyInstallError(error: unknown, name: string): ClassifiedInstallError {
	const raw = errorText(error);
	const kind = classifyInstallKind(raw);
	return {
		kind,
		message: installErrorMessage(kind, name),
		details: dedupeInstallLog(raw),
		debugLogPath: extractDebugLogPath(raw),
		// A 404 can clear once the Store's index is refreshed, so every kind
		// keeps Retry except an install the server doesn't run at all.
		retryable: kind !== 'unavailable',
	};
}
