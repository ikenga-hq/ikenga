export type PaneId = string;

// Pane view kinds. `mini-app` was removed in the strip-down — media tooling
// (storyboard/video-engine/hyperframes/canvas/image-generator) now lives in
// app pkgs that mount via /pkg/$pkgId/* routes.
export type PaneView = (
	| { kind: 'route'; path: string }
	| { kind: 'terminal'; sessionId: string }
	| { kind: 'artifact'; path: string; line?: number; col?: number }
	// Unified Artifact Studio with three densities:
	//   grid    — folder (Lightroom-style contact sheet of artifact thumbnails)
	//   loupe   — single artifact (preview + version strip + right rail)
	//   compare — two artifacts side-by-side (requires `vs`)
	// `path` is the canonical target; `vs` is the second artifact at
	// compare density. See plans/shell/2026-05-16-artifact-studio-unified.md.
	| {
			kind: 'artifact-studio';
			path: string;
			density: 'grid' | 'loupe' | 'compare';
			vs?: string;
			/** Terminal tab id (TerminalTab.id, not ptyId) attached to this
			 *  Studio pane. The tab id is stable across reloads; the ptyId is
			 *  looked up from useTerminalStore at mount time. Excluded from
			 *  `viewsMatch` so attachment is metadata, not identity. */
			attachedTerminalId?: string;
	  }
	| { kind: 'scratchpad'; scope: string; name: string }
) & { pinned?: boolean };

export type PaneDirection = 'horizontal' | 'vertical';

export interface SplitNode {
	type: 'split';
	direction: PaneDirection;
	children: PaneNode[];
	sizes: number[];
}

export interface LeafNode {
	type: 'leaf';
	id: PaneId;
	tabs: PaneView[];
	activeTabIdx: number;
}

export type PaneNode = SplitNode | LeafNode;

export const MAX_LEAVES = 6;
