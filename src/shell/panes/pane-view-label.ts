// Pure label/subtitle logic for a `PaneView`, split out of `pane-views.tsx`
// so that computing a tab's title doesn't drag in `PaneBody`'s full view
// registry (route/terminal/artifact/artifact-studio/scratchpad components,
// and everything *they* import). `pane-views.tsx` re-exports everything here
// for existing callers; new callers that only need label text (tab strip,
// address bar, ⌘K switcher) import this module directly.

import type { PaneView } from '@/lib/panes/types';
import type { TerminalTitleResolver } from '@/terminal/use-terminal-titles';
import type { PaneDisplayNameResolver } from './use-pane-display-names';

// `/pkg/<id>` (optionally with a sub-path) and `/ngwa/item/<id>` are the two
// route shapes whose last path segment is a reverse-DNS id, not a human
// label — `com.ikenga.studio`, not "Studio". `resolveDisplayName` (below)
// looks the id up; these just carry the id out of the path.
const PKG_ROUTE_ID_RE = /^\/pkg\/([^/]+)(?:\/.*)?$/;
const NGWA_ITEM_ROUTE_ID_RE = /^\/ngwa\/item\/([^/]+)$/;

function routeIdFromPath(path: string): string | null {
	const match = PKG_ROUTE_ID_RE.exec(path) ?? NGWA_ITEM_ROUTE_ID_RE.exec(path);
	return match ? decodeURIComponent(match[1]) : null;
}

export interface RouteLabelInfo {
	label: string;
	/** True when `label` came from `resolveDisplayName` rather than a raw path
	 *  segment or id. Callers use this to skip title-casing: a resolved
	 *  manifest/display name is already correctly cased (CSS `capitalize`
	 *  would mis-render e.g. "iOS Bridge" as "IOS Bridge"), and either way a
	 *  dotted id should never be capitalized either — see `shouldCapitalizeLabel`. */
	resolved: boolean;
}

/** Exported so callers that need to know *whether* a route label resolved
 *  (not just its text) — the ⌘K switcher's "Route · <name>" vs. the raw
 *  "Route <path>" — don't have to re-run the id-extraction regexes themselves. */
export function routeLabelInfo(
	path: string,
	resolveDisplayName?: PaneDisplayNameResolver
): RouteLabelInfo {
	const id = routeIdFromPath(path);
	if (id) {
		// A pkg/ngwa-item id is a reverse-DNS string, not dash-separated words —
		// unlike the generic fallback below, an unresolved id is shown exactly
		// as-is (no dash→space), since "com.ikenga.unknown-pkg" isn't "unknown
		// pkg" with punctuation removed.
		const resolvedName = resolveDisplayName?.(id);
		return resolvedName ? { label: resolvedName, resolved: true } : { label: id, resolved: false };
	}
	const segs = path.split('/').filter(Boolean);
	if (segs.length === 0) return { label: 'Dashboard', resolved: false };
	return { label: segs[segs.length - 1].replace(/-/g, ' '), resolved: false };
}

/**
 * Tab label for a view.
 *
 * `resolveTerminal` is how a terminal tab gets a real name (`claude · shell`)
 * instead of the constant "Terminal" — it needs the session store plus the live
 * foreground poll, neither of which belongs in a pure function. Callers that
 * render tab strips pass `useTerminalTitles()`; everyone else omits it and gets
 * the old constant, which is still correct, just uninformative.
 *
 * `resolveDisplayName` is the same idea for a `route` view whose path is
 * `/pkg/<id>` or `/ngwa/item/<id>`: pass `usePaneDisplayNameResolver()` to get
 * "Studio" instead of "com.ikenga.studio"; omit it and the raw id shows
 * (still correct, just the id).
 */
export function viewLabel(
	view: PaneView,
	resolveTerminal?: TerminalTitleResolver,
	resolveDisplayName?: PaneDisplayNameResolver
): string {
	switch (view.kind) {
		case 'route':
			return routeLabelInfo(view.path, resolveDisplayName).label;
		case 'terminal':
			return resolveTerminal?.(view.sessionId)?.label ?? 'Terminal';
		case 'artifact': {
			const name = view.path.split('/').filter(Boolean).pop();
			return name ?? 'Artifact';
		}
		case 'artifact-studio': {
			const name = view.path.split('/').filter(Boolean).pop();
			const prefix = view.density === 'grid' ? 'Grid' : 'Studio';
			return `${prefix} · ${name ?? (view.density === 'grid' ? 'folder' : 'artifact')}`;
		}
		case 'scratchpad':
			return view.name;
	}
}

/**
 * Whether a tab strip should CSS `capitalize` this view's label.
 *
 * Terminal labels are real command + directory names (`claude · shell`) —
 * title-casing them would render "Claude · Shell" and misspell anything
 * lowercase by convention. A dotted label is either an unresolved
 * reverse-DNS id (`com.ikenga.studio`, where CSS `capitalize` treats `.` as
 * a word boundary and renders "Com.Ikenga.Studio") or a resolved display
 * name that happens to contain a literal dot — neither should be title-cased.
 * A resolved display name without a dot is excluded too: it's already
 * correctly cased by whoever named the pkg/ngwa item.
 */
export function shouldCapitalizeLabel(
	view: PaneView,
	label: string,
	resolveDisplayName?: PaneDisplayNameResolver
): boolean {
	if (view.kind === 'terminal') return false;
	if (label.includes('.')) return false;
	if (view.kind === 'route' && routeLabelInfo(view.path, resolveDisplayName).resolved) return false;
	return true;
}

export function viewSubtitle(view: PaneView, resolveTerminal?: TerminalTitleResolver): string {
	switch (view.kind) {
		case 'route':
			return view.path || '/';
		case 'terminal':
			// The terminal tooltip is multi-line — label, full cwd, argv, agent
			// label — so the tab's hover carries everything the label had to drop.
			return resolveTerminal?.(view.sessionId)?.tooltip ?? `session: ${view.sessionId}`;
		case 'artifact':
			return view.path;
		case 'artifact-studio':
			return view.density === 'compare' && view.vs ? `${view.path} ↔ ${view.vs}` : view.path;
		case 'scratchpad':
			return view.scope;
	}
}
