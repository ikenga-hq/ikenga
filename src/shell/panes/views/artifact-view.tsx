import { ArrowUpRight } from 'lucide-react';
import { useCallback } from 'react';
import { IconButton } from '@/components/ui/icon-button';
import { spawnWindow } from '@/lib/tauri-cmd';
import {
	markSurfaceDetached,
	syncDetachedSurfaces,
	useIsSurfaceDetached,
} from '@/lib/window/detached-surfaces';
import { ViewerRouter } from '@/viewer/auto-router';
import { DetachedSurfacePlaceholder } from './detached-placeholder';

interface ArtifactViewProps {
	path: string;
	/** Forwarded to HtmlFrame for iyke iframe bridging. */
	paneId?: string;
	line?: number;
	col?: number;
}

// Thin pane-registry shim. Routing + chrome live in src/viewer/auto-router —
// this module exists so the pane store's `kind: 'artifact'` view continues to
// resolve to a stable export.
export function ArtifactView({ path, paneId, line, col }: ArtifactViewProps) {
	// Pop-out: spawn a thin single-surface viewer window for this file.
	// The path is encoded in the surface_set entry ("viewer:<path>") so the
	// detached ViewerSurface can extract it from ctx.surfaces[0].
	// First-colon split only, so absolute paths starting with "/" survive.
	const surfaceId = `viewer:${path}`;
	const isDetached = useIsSurfaceDetached(surfaceId);
	const handlePopOut = useCallback(() => {
		const label = `detached-viewer-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
		// Optimistically mark detached so this pane swaps to the placeholder
		// immediately instead of briefly duplicating the viewer.
		markSurfaceDetached(surfaceId, label);
		void spawnWindow({
			label,
			kind: 'single-surface',
			surface_set: [surfaceId],
			project_id: null,
			layout_key: label,
		}).catch((e) => {
			console.warn('pop-out viewer:', e);
			// Reconcile the optimistic mark if the window never opened.
			void syncDetachedSurfaces();
		});
	}, [surfaceId]);

	// Popped out into its own window — render the reclaim placeholder, not the
	// live duplicate.
	if (isDetached) {
		return <DetachedSurfacePlaceholder surfaceId={surfaceId} noun="file" />;
	}

	return (
		<div className="relative flex h-full w-full flex-col">
			{/* Pop-out affordance — floated top-right over the viewer chrome.
			    Positioned absolute so it overlays the ViewerRouter's own header
			    without requiring ViewerRouter to know about multi-window. */}
			<div className="absolute right-2 top-1 z-10">
				<IconButton
					onClick={handlePopOut}
					title="Pop out — open this file in a detached viewer window"
					aria-label="Pop out viewer"
					className="bg-background/80 backdrop-blur-sm"
				>
					<ArrowUpRight className="h-3.5 w-3.5" />
				</IconButton>
			</div>
			<ViewerRouter path={path} source="pane" paneId={paneId} line={line} col={col} editable />
		</div>
	);
}
