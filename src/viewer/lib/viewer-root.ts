// The viewer mount root for an HTML page, bounded by the page's project.
//
// `pickViewerRoot` widens the root by the page's own `../` references, and the
// page is untrusted, so the widening is capped at the project root taken from
// the project list. A page in no project (or when the list cannot be read)
// gets its own directory only: fail closed. The daemon re-checks the bound.
//
// Browser sessions only. The desktop app serves from its own in-process viewer
// server (`viewer_server/mod.rs`, a separate path with no project bound), so
// its behaviour is deliberately unchanged here: the whole path is the bound.

import { isRemoteWebSession, projectList } from '@/lib/tauri-cmd';
import { pickViewerRoot, projectRootOf, type RelativeRoot } from './relative-root';

export async function resolveViewerRoot(path: string, html: string): Promise<RelativeRoot> {
	if (!isRemoteWebSession()) return pickViewerRoot(path, html, '/');
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
