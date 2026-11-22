// WP-12: a member who enters an artifact-scope share lands on the shared file.
//
// `openShare` (shared-with-you.tsx) and the invite page reload the app into
// share mode, so the intent is carried across the reload in sessionStorage
// and consumed once by `ShareModeBanner` on the reloaded page (never on the
// page that set it), after the workspace has restored its layout.

import { whenLayoutRestored } from '@/lib/panes/layout-restored';
import { type FileEntry, projectGetActive } from '@/lib/tauri-cmd';
import type { ShareSelection } from '@/lib/transport';

export const LAND_KEY = 'ikenga_share_land';

/** How long the landing waits for the restored layout before opening anyway. */
const LAYOUT_WAIT_MS = 15_000;

/** The project folder joined to a project-relative artifact path (`/`-separated). */
export function joinProjectPath(root: string, rel: string): string {
	const r = root.replace(/[\\/]+$/, '');
	const p = rel.replace(/^\.?[\\/]+/, '');
	return `${r}/${p}`;
}

/**
 * In an artifact-scope share, the one Files row for the shared file under the
 * project root `rootPath`, or `null` (no artifact share, or another root).
 * The member may not list the project (the server refuses `fs_list` there),
 * so the Files panel shows this row instead of an error.
 */
export function sharedArtifactEntry(
	rootPath: string,
	share: ShareSelection | null
): FileEntry | null {
	if (!share || share.scope !== 'artifact' || !share.artifactPath) return null;
	const path = joinProjectPath(rootPath, share.artifactPath);
	const root = rootPath.replace(/[\\/]+$/, '');
	if (!root || path.slice(0, root.length + 1) !== `${root}/`) return null;
	return {
		path,
		name: share.artifactPath.replace(/^\.?[\\/]+/, ''),
		isDir: false,
		size: 0,
		modifiedMs: 0,
	};
}

/** Remember that the next load of this share should open its artifact. */
export function markShareLanding(projectKey: string, scope: 'project' | 'artifact'): void {
	if (typeof sessionStorage === 'undefined' || scope !== 'artifact') return;
	try {
		sessionStorage.setItem(LAND_KEY, projectKey);
	} catch {
		// Storage unavailable: the member opens the file from the Files panel.
	}
}

function readMarker(): string | null {
	if (typeof sessionStorage === 'undefined') return null;
	try {
		return sessionStorage.getItem(LAND_KEY);
	} catch {
		return null;
	}
}

/**
 * The marker as this page load found it. The landing belongs to the NEXT
 * load: the invite page (and `openShare`) enter share mode, which the
 * already-mounted `ShareModeBanner` hears at once, then mark and reload.
 * Read live, the marker was consumed by that page, whose open was then
 * thrown away by its own reload, and the reloaded app never landed.
 */
let bootMarker = readMarker();

/** True once per `markShareLanding` for this share, on the load after the
 *  mark; clears the marker. */
export function consumeShareLanding(projectKey: string): boolean {
	if (bootMarker !== projectKey) return false;
	bootMarker = null;
	try {
		if (sessionStorage.getItem(LAND_KEY) === projectKey) sessionStorage.removeItem(LAND_KEY);
	} catch {
		// The marker stays; this page load has used it either way.
	}
	return true;
}

/** Tests only: read the marker again, as a fresh page load would. */
export function reloadShareLandingForTests(): void {
	bootMarker = readMarker();
}

/** Open `artifactPath` (project-relative) of the shared project in the viewer. */
export async function openSharedArtifact(artifactPath: string): Promise<boolean> {
	const project = await projectGetActive();
	if (!project.root_path) return false;
	const path = joinProjectPath(project.root_path, artifactPath);
	// Imported here, not at module level: this file is reachable from the
	// sign-in overlay, and the pane store's imports open the events socket at
	// load, before the server tier (and so the right credential) is known.
	const { usePaneStore } = await import('@/lib/panes/pane-store');
	// The workspace restores the saved layout right after the reload and
	// replaces the whole pane tree when it does (in a share that is the
	// default layout: the member's own one is not readable there). A tab added
	// before that lands is wiped, so wait for it; past the bound, open anyway.
	await whenLayoutRestored(LAYOUT_WAIT_MS);
	for (let i = 0; i < 20; i++) {
		const { focusedId, addTab } = usePaneStore.getState();
		if (focusedId) {
			addTab(focusedId, { kind: 'artifact', path });
			return true;
		}
		await new Promise((r) => setTimeout(r, 250));
	}
	return false;
}
