// WP-12: a member who enters an artifact-scope share lands on the shared file.
//
// `openShare` (shared-with-you.tsx) reloads the app into share mode, so the
// intent is carried across the reload in sessionStorage and consumed once by
// `ShareModeBanner`, which runs inside the session chrome after the reload.

import { usePaneStore } from '@/lib/panes/pane-store';
import { projectGetActive } from '@/lib/tauri-cmd';

export const LAND_KEY = 'ikenga_share_land';

/** The project folder joined to a project-relative artifact path (`/`-separated). */
export function joinProjectPath(root: string, rel: string): string {
	const r = root.replace(/[\\/]+$/, '');
	const p = rel.replace(/^\.?[\\/]+/, '');
	return `${r}/${p}`;
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

/** True once per `markShareLanding` for this share; clears the marker. */
export function consumeShareLanding(projectKey: string): boolean {
	if (typeof sessionStorage === 'undefined') return false;
	try {
		if (sessionStorage.getItem(LAND_KEY) !== projectKey) return false;
		sessionStorage.removeItem(LAND_KEY);
		return true;
	} catch {
		return false;
	}
}

/** Open `artifactPath` (project-relative) of the shared project in the viewer. */
export async function openSharedArtifact(artifactPath: string): Promise<boolean> {
	const project = await projectGetActive();
	if (!project.root_path) return false;
	const path = joinProjectPath(project.root_path, artifactPath);
	// The pane store may still be hydrating right after the reload.
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
