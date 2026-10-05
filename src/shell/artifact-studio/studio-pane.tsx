// Artifact Studio — built-in shell mini-app (NOT a pkgs/* package).
//
// Single entry point for the three Studio densities:
//   grid     — Lightroom-style contact-sheet over a folder
//   loupe    — single-artifact deep view (preview + version strip + rail)
//   compare  — two artifacts side-by-side (Phase 3)
//
// Density is set at addTab-site (see `@/lib/panes/artifact-studio-route`)
// and travels with the pane view. The actual layout per density lives
// in `density/{grid,loupe,compare}.tsx`.
//
// User-facing copy says "Artifact grid" for the whole surface (grid, loupe
// and compare are its views); internally we use `artifact-studio` (kebab) /
// `ArtifactStudio` (component) — never the bare word "Studio" in copy since
// that collides with pkgs/studio/.

import { StudioGrid } from './density/grid';
import { StudioLoupe } from './density/loupe';
import { StudioCompare } from './density/compare';

type Density = 'grid' | 'loupe' | 'compare';

interface ArtifactStudioProps {
	path: string;
	paneId: string;
	density: Density;
	vs?: string;
	attachedTerminalId?: string;
}

export function ArtifactStudio({
	path,
	paneId,
	density,
	vs,
	attachedTerminalId,
}: ArtifactStudioProps) {
	switch (density) {
		case 'grid':
			return <StudioGrid path={path} paneId={paneId} />;
		case 'loupe':
			return <StudioLoupe path={path} paneId={paneId} attachedTerminalId={attachedTerminalId} />;
		case 'compare':
			// `vs` is required for compare density. When missing (defensive,
			// shouldn't happen via the route resolver), fall back to loupe.
			if (!vs)
				return <StudioLoupe path={path} paneId={paneId} attachedTerminalId={attachedTerminalId} />;
			return <StudioCompare paneId={paneId} a={path} b={vs} />;
	}
}
