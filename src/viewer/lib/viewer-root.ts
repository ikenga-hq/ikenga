// The viewer mount root for an HTML page, bounded by the page's project.
//
// `pickViewerRoot` widens the root by the page's own `../` references, and the
// page is untrusted, so the widening is capped at the project root taken from
// the project list. A page in no project (or when the list cannot be read)
// gets its own directory only: fail closed. The host re-checks the bound from
// its own project registry (the daemon, and the desktop's `viewer_serve`), and
// additionally serves only the previewed file when that bound is the home
// directory or above, so a page directly in `~` still previews.

import { projectList, type ViewerHandle, viewerServe } from '@/lib/tauri-cmd';
import { basename, dirname } from './path';
import { pickViewerRoot, projectRootOf, type RelativeRoot } from './relative-root';

export async function resolveViewerRoot(path: string, html: string): Promise<RelativeRoot> {
	let boundary: string | null = null;
	try {
		const projects = await projectList(false);
		boundary = projectRootOf(
			path,
			projects.map((p) => p.root_path)
		);
	} catch {
		// No project list: no widening.
	}
	return pickViewerRoot(path, html, boundary);
}

/**
 * Mount the page's viewer root and return the handle plus the file to append to
 * its URL. The project list cannot say that a project is rooted at the home
 * directory (which bounds nothing, so the host bounds the page by its own
 * directory instead and refuses the wider root), so a refused widened root is
 * retried once with the page's own directory.
 */
export async function mountViewerRoot(
	path: string,
	html: string
): Promise<{ handle: ViewerHandle; file: string }> {
	const { root, file } = await resolveViewerRoot(path, html);
	try {
		return { handle: await viewerServe(root, path), file };
	} catch (err) {
		const own = dirname(path);
		if (root === own) throw err;
		return { handle: await viewerServe(own, path), file: basename(path) };
	}
}
