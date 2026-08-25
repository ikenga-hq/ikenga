// Pane address helpers.
//
// A "pane address" is the displayable location string shown in the pane URL
// bar. Only views that have a natural path/URL get an address (and a URL
// bar) — terminal panels are address-less.
//
// The parser turns a typed-in address back into a `PaneView` candidate so the
// URL bar can navigate. Unknown shapes return null and the toolbar should
// reject the input visually rather than navigate.

import type { PaneView } from './types';

/** Returns the displayable address for a view, or null if the view has no
 * natural URL/path (terminal). */
export function getPaneAddress(view: PaneView): string | null {
	switch (view.kind) {
		case 'route':
			return view.path || '/';
		case 'artifact': {
			const loc = view.line ? (view.col ? `:${view.line}:${view.col}` : `:${view.line}`) : '';
			return `${view.path}${loc}`;
		}
		case 'artifact-studio': {
			// At compare density the address carries `?vs=<other>` so the
			// resolver can round-trip back to the same view from a URL bar.
			// Loupe density carries `?term=<tabId>` when a terminal is
			// attached so reload + history navigation preserve attachment.
			const params: string[] = [];
			if (view.density === 'compare' && view.vs) params.push(`vs=${view.vs}`);
			if (view.attachedTerminalId) params.push(`term=${view.attachedTerminalId}`);
			return params.length > 0 ? `${view.path}?${params.join('&')}` : view.path;
		}
		case 'terminal':
		case 'scratchpad':
			return null;
	}
}

/** Whether a view kind shows the URL bar at all. */
export function hasAddressBar(view: PaneView): boolean {
	return getPaneAddress(view) !== null;
}

/** Format the address for display. For artifact views whose path matches a
 *  pinned artifact with a stable `manifest_id`, return the canonical
 *  `ikenga://artifact/<id>` URI instead of the on-disk path. Other views
 *  (route, plain artifacts) round-trip through `getPaneAddress` unchanged.
 *
 *  Pure, by-value: the caller passes a `pathToManifestId` map so the hook
 *  in `pane-address-bar.tsx` controls how/when it's rebuilt (and so unit
 *  tests don't have to fake the pins-store). */
export function formatPaneAddressForDisplay(
	view: PaneView,
	pathToManifestId: ReadonlyMap<string, string>
): string | null {
	if (view.kind !== 'artifact') return getPaneAddress(view);
	const manifestId = pathToManifestId.get(view.path);
	if (manifestId) return `ikenga://artifact/${manifestId}`;
	return view.path;
}

/**
 * Parse a user-typed address into a candidate view. Returns null on parse
 * failure so the UI can ring red.
 *
 * Rules (first match wins):
 *   - `http://` / `https://`            → artifact (auto-router handles URLs)
 *   - `ikenga://artifact/<id-or-path>`  → artifact (path = the suffix)
 *   - `/route...`                        → route view (path-based router)
 *   - absolute fs path (`/home/...` or `C:\...`) → artifact
 *   - relative path containing a dot or slash    → artifact
 */
export function parsePaneAddress(input: string): PaneView | null {
	const raw = input.trim();
	if (!raw) return null;

	if (raw.startsWith('http://') || raw.startsWith('https://')) {
		// External URLs are rendered via the artifact viewer's html-frame
		// renderer. Path-shaped storage keeps the existing view kind union
		// intact; the URL bar treats it as an address either way.
		return { kind: 'artifact', path: raw };
	}

	if (raw.startsWith('ikenga://artifact/')) {
		const rest = raw.slice('ikenga://artifact/'.length);
		if (!rest) return null;
		// Keep the literal `ikenga://artifact/<id>` in the path. The URI is a
		// pinned-artifact lookup key, not a filesystem path; an async resolver
		// (`resolveArtifactAddress` in `./pane-address-resolver`) swaps it for
		// the on-disk path before the artifact pane mounts. Stripping here
		// would lose the scheme and we'd try to open a file at "<id>".
		return { kind: 'artifact', path: raw };
	}

	// Reject other unknown schemes outright — `foo://bar`, `mailto:...`, etc.
	// We don't currently have a view that handles them and silently turning
	// them into a route or artifact would surprise the user.
	if (/^[a-z][a-z0-9+.-]*:\/\//i.test(raw) || /^[a-z][a-z0-9+.-]*:/i.test(raw.slice(0, 12))) {
		// Allow Windows drive-letter paths (`C:\`, `D:/`) which match the second
		// pattern but are filesystem paths, not URI schemes.
		if (!/^[a-zA-Z]:[\\/]/.test(raw)) return null;
	}

	// Windows drive-letter path → artifact.
	if (/^[a-zA-Z]:[\\/]/.test(raw)) {
		return { kind: 'artifact', path: raw };
	}

	if (raw.startsWith('/')) {
		// Heuristic: a leading-slash string with a file extension or a known
		// fs prefix is a filesystem path; otherwise it's a route.
		if (looksLikeFsPath(raw)) return { kind: 'artifact', path: raw };
		return { kind: 'route', path: raw };
	}

	// Relative-ish input that contains a slash or a dot is treated as a
	// filesystem-style artifact path. Anything else (a single bare word) is
	// ambiguous — reject it.
	if (raw.includes('/') || raw.includes('.')) {
		return { kind: 'artifact', path: raw };
	}

	return null;
}

const FS_PREFIXES = ['/home/', '/Users/', '/tmp/', '/var/', '/opt/', '/mnt/', '/etc/', '/private/'];

function looksLikeFsPath(raw: string): boolean {
	if (FS_PREFIXES.some((p) => raw.startsWith(p))) return true;
	// A trailing file extension on the last segment is a strong signal.
	const last = raw.split('/').pop() ?? '';
	if (/\.[a-zA-Z0-9]{1,8}$/.test(last)) return true;
	return false;
}
