// Decide the directory the viewer's static server should be rooted at.
//
// `<iframe src>` resolves relative URLs against the iframe origin, so any
// `<link href="../foo">` / `<script src="../foo">` inside the HTML must be
// reachable under the served root. Naïvely serving the file's parent breaks
// shared-asset designs (`../_shared/tokens.css` etc.). We scan the HTML for
// the deepest `..` ascent in any link/script/img reference and serve from
// that ancestor instead.
//
// The ascent is capped at the file's project root (`boundary`), never above
// it: the HTML is untrusted (a teammate's file in a shared project), and a
// `src="../../../.ssh/id_rsa"` must not widen the mount to the whole home. A
// file in no project (`boundary` null) gets no widening at all: its own
// directory. The daemon enforces the same bound from its own project registry
// and refuses a root above it; this clamp only keeps the preview working (and
// the error away) when a page reaches past the project.
//
// Returns `{ root, file }` — the root to pass to `viewer_serve` and the
// relative path (from that root) to append to the served URL.

import { dirname } from './path';

export interface RelativeRoot {
	root: string;
	file: string;
}

const REF_ATTR_RE = /(?:href|src)\s*=\s*(?:"([^"]+)"|'([^']+)')/gi;

export function pickViewerRoot(
	htmlPath: string,
	html: string,
	boundary: string | null = null
): RelativeRoot {
	const fileDir = dirname(htmlPath);
	const fileName = htmlPath.slice(fileDir.length + 1);

	let maxAscent = 0;
	for (const m of html.matchAll(REF_ATTR_RE)) {
		const value = (m[1] ?? m[2] ?? '').trim();
		if (
			!value ||
			/^[a-z][a-z0-9+.-]*:/i.test(value) ||
			value.startsWith('//') ||
			value.startsWith('#') ||
			value.startsWith('/') ||
			value.startsWith('data:')
		) {
			continue;
		}
		const ascent = countLeadingAscent(value);
		if (ascent > maxAscent) maxAscent = ascent;
	}

	if (maxAscent === 0) {
		return { root: fileDir, file: fileName };
	}

	const dirSegments = fileDir.split('/').filter((s) => s.length > 0);
	// How many directories between the file and the project root; 0 without one.
	const room = boundary === null ? 0 : ascentRoom(fileDir, boundary);
	const ascent = Math.min(maxAscent, dirSegments.length, room);
	if (ascent === 0) return { root: fileDir, file: fileName };
	const rootSegments = dirSegments.slice(0, dirSegments.length - ascent);
	const root = '/' + rootSegments.join('/');
	const fileSegments = dirSegments.slice(dirSegments.length - ascent).concat(fileName);
	const file = fileSegments.join('/');
	return { root, file };
}

function countLeadingAscent(value: string): number {
	let count = 0;
	let rest = value;
	while (rest.startsWith('../')) {
		count++;
		rest = rest.slice(3);
	}
	return count;
}

/** Directories from `fileDir` up to `boundary` (0 if `fileDir` is not inside it). */
function ascentRoom(fileDir: string, boundary: string): number {
	const dir = fileDir.split('/').filter((s) => s.length > 0);
	const top = boundary.split('/').filter((s) => s.length > 0);
	if (top.length > dir.length) return 0;
	for (let i = 0; i < top.length; i++) if (dir[i] !== top[i]) return 0;
	return dir.length - top.length;
}

/**
 * The deepest project root (from the project list) that contains `path`, or
 * null when it is in no project. Mirrors the daemon's `viewer::project_root_of`
 * for the client-side clamp; the daemon's own answer is the one that counts
 * (it also ignores a project rooted at the whole home, which this cannot know).
 */
export function projectRootOf(
	path: string,
	roots: ReadonlyArray<string | null | undefined>
): string | null {
	const segs = (p: string) => p.split('/').filter((s) => s.length > 0);
	const file = segs(path);
	const within = (inner: string[], outer: string[]) =>
		outer.length <= inner.length && outer.every((s, i) => inner[i] === s);
	let best: string[] | null = null;
	for (const r of roots) {
		if (!r?.trim()) continue;
		const root = segs(r);
		if (root.length === 0) continue;
		if (!within(file.slice(0, -1), root)) continue;
		if (!best || root.length > best.length) best = root;
	}
	return best ? `/${best.join('/')}` : null;
}
