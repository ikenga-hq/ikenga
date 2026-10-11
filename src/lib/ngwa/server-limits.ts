// "Unavailable by design" vs "failed", for the Ngwa snapshot's sources.
//
// A source the host does not run at all (the headless daemon has no pkg
// sidecar supervisor, no trust store, no engine_assets registry) comes back
// `ok: false` with `unavailable: true` — a typed field set by the daemon
// (`NgwaSourceHealth::unavailable`, `server::shared::ngwa`). That is an
// expected limitation of the server, not an error: it reads as ONE muted info
// line, never a red banner. A source that was run and failed has no such
// field and keeps the error styling. The prose in `error` is only ever the
// reason shown in the details; nothing here matches on it.

import type { NgwaSnapshot } from '@ikenga/contract';

/** A snapshot source's health as the daemon sends it: the contract's three
 *  fields plus the daemon's typed `unavailable` marker (absent on the desktop
 *  and on any source that merely failed). Not in the contract's schema. */
export type NgwaSourceHealthView = NgwaSnapshot['sources'][keyof NgwaSnapshot['sources']] & {
	unavailable?: boolean;
};

/** Whether the host does not run this source at all. */
export function isServerLimit(health: { unavailable?: boolean } | null | undefined): boolean {
	return health?.unavailable === true;
}

/** What each source is called in the notice. */
const SOURCE_LABELS: Record<string, string> = {
	kernel: 'package runtime',
	trust: 'trust',
	engine_assets: 'engine placement',
	usage: 'session usage',
	oba: 'the vault',
	engine_config: 'engine config',
};

/** Notice order (the example in the design: runtime, trust, placement). */
const SOURCE_ORDER = ['kernel', 'trust', 'engine_assets', 'usage', 'oba', 'engine_config'];

export function sourceLabel(source: string): string {
	return SOURCE_LABELS[source] ?? source.replace(/_/g, ' ');
}

export interface ServerLimit {
	source: string;
	label: string;
	/** The daemon's one-line reason, without its "<what> is not available on
	 *  this server:" lead-in. */
	reason: string;
}

/** The part of a daemon reason after its "<what> … : " head, so the details
 *  list does not repeat the label. Falls back to the whole text. */
function reasonOf(error: string | null): string {
	const text = (error ?? '').trim();
	if (!text) return 'not run on this server';
	const at = text.indexOf(': ');
	const rest = at >= 0 ? text.slice(at + 2).trim() : '';
	if (!rest) return text;
	return rest.charAt(0).toUpperCase() + rest.slice(1);
}

/** The sources the host does not run, in notice order, from
 *  `useNgwaSnapshot().unreadableSources`. */
export function serverLimits(
	sources: ReadonlyArray<{ source: string; error: string | null; unavailable?: boolean }>
): ServerLimit[] {
	const rank = (s: string) => {
		const i = SOURCE_ORDER.indexOf(s);
		return i < 0 ? SOURCE_ORDER.length : i;
	};
	return sources
		.filter((s) => s.unavailable === true)
		.sort((a, b) => rank(a.source) - rank(b.source))
		.map((s) => ({ source: s.source, label: sourceLabel(s.source), reason: reasonOf(s.error) }));
}

/** "a", "a and b", "a, b and c". */
function joinList(items: string[]): string {
	if (items.length <= 1) return items[0] ?? '';
	return `${items.slice(0, -1).join(', ')} and ${items[items.length - 1]}`;
}

/** The one info line: "On this server: package runtime, trust and engine
 *  placement are desktop-only". */
export function serverLimitsLine(limits: ReadonlyArray<ServerLimit>): string {
	if (limits.length === 0) return '';
	return `On this server: ${joinList(limits.map((l) => l.label))} ${
		limits.length === 1 ? 'is' : 'are'
	} desktop-only`;
}
