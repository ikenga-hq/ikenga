// D-08 `artifact-history` — version data for the drawer
// (designs/pane-chrome.html?state=artifact-history, the `VERSIONS` array).
//
// The design draws two real sources — git history and "viewer snapshots" —
// alongside the working tree. Neither has a backend in this codebase today:
// there is no git-log command (no `Command::new("git")` / `git2` use outside
// `claude_store/install.rs`, which is unrelated) and no snapshot store (no
// table or command with "snapshot" in the name that means what this screen
// needs). Re-grepped per G-54 rather than assumed from the brief.
//
// Adding either is real, un-reviewable-before-merge Rust surface (process
// exec for git-log; a new persisted store for snapshots) that this WP's
// scope — and DEC-50's no-build/no-test constraint, which leaves no way to
// verify new Rust compiles before it lands — doesn't cover. So this hook
// ships the real shape the drawer needs and returns it honestly empty; the
// UI (`version-history-panel.tsx`) is fully wired against it and needs no
// changes the day a git-log / snapshot command exists.

export type ArtifactVersionKind = 'now' | 'git' | 'snapshot';

export interface ArtifactVersion {
	id: string;
	kind: ArtifactVersionKind;
	label: string;
	when: string;
	who?: string;
	note?: string;
}

export interface ArtifactVersionsResult {
	versions: ArtifactVersion[];
	loading: boolean;
}

const WORKING_TREE: ArtifactVersion = {
	id: 'v-now',
	kind: 'now',
	label: 'Working tree',
	when: 'now',
	who: 'on disk',
};

/** `path` is unused today (no data source reads it yet) — kept in the
 *  signature so callers don't need to change when a real source lands. */
export function useArtifactVersions(_path: string): ArtifactVersionsResult {
	return { versions: [WORKING_TREE], loading: false };
}
