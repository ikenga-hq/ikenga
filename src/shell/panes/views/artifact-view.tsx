import { ArrowUpRight } from 'lucide-react';
import { lazy, Suspense, useCallback, useEffect, useState } from 'react';
import { IconButton } from '@/components/ui/icon-button';
import { spawnWindow } from '@/lib/tauri-cmd';
import {
	markSurfaceDetached,
	syncDetachedSurfaces,
	useIsSurfaceDetached,
} from '@/lib/window/detached-surfaces';
import { ViewerRouter } from '@/viewer/auto-router';
import { ArtifactInfoStrip } from '@/viewer/chrome/artifact-info-strip';
import { ArtifactStoppedPlate } from '@/viewer/chrome/artifact-stopped-plate';
import { useArtifactDiskWatch } from '@/viewer/chrome/use-artifact-disk-watch';
import { useViewerServerHealth } from '@/viewer/chrome/use-viewer-server-health';
import { VersionHistoryPanel } from '@/viewer/history/version-history-panel';
import { useViewerPaneState } from '@/viewer/viewer-pane-state';
import { DetachedSurfacePlaceholder } from './detached-placeholder';

// Split out of the main bundle exactly like auto-router.tsx does — "Open
// source" is an occasional action, Shiki is ~300KB.
const CodeView = lazy(() =>
	import('@/viewer/renderers/code-view').then((m) => ({ default: m.CodeView }))
);

interface ArtifactViewProps {
	path: string;
	/** Forwarded to HtmlFrame for iyke iframe bridging, and used as the key
	 *  for this pane's D-08 chrome state (zoom / device / variant). */
	paneId?: string;
	line?: number;
	col?: number;
}

// Thin pane-registry shim. Routing + renderer chrome live in
// src/viewer/auto-router; D-08's pane-level chrome (info strip, device
// width, source split, version history) lives here, one level up, because
// it's the same for every renderer and the merged `PaneAddressBar` (the
// pane's *only* chrome row, D-08) already renders the path — so this always
// mounts ViewerRouter `chromeless` instead of letting it draw its own
// (redundant) filename/mime header.
export function ArtifactView({ path, paneId, line, col }: ArtifactViewProps) {
	const stateKey = paneId ?? path;

	// Dispatch editor jump event when line/col are provided (WP-05 / T-04)
	useEffect(() => {
		if (line !== undefined) {
			window.dispatchEvent(
				new CustomEvent('ikenga:editor-jump', {
					detail: { path, line, col },
				})
			);
		}
	}, [path, line, col]);

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

	// D-08 chrome state (designs/pane-chrome.html) — zoom / device preset /
	// which variant (default renderer, source split, history drawer).
	const zoom = useViewerPaneState((s) => s.forPane(stateKey).zoom);
	const device = useViewerPaneState((s) => s.forPane(stateKey).device);
	const variant = useViewerPaneState((s) => s.forPane(stateKey).variant);
	const setVariant = useViewerPaneState((s) => s.setVariant);
	const resetViewerState = useViewerPaneState((s) => s.reset);

	const { changed, reloadKey, dismiss } = useArtifactDiskWatch(path);
	const { stopped, restart } = useViewerServerHealth(path);
	// D-08 `artifact-stopped` is strip + plate: the dismissible "viewer server
	// stopped" strip above the full-content plate. Dismissal lasts until the
	// server comes back (or the pane moves to another file).
	const [stoppedStripDismissed, setStoppedStripDismissed] = useState(false);
	useEffect(() => {
		if (!stopped) setStoppedStripDismissed(false);
	}, [stopped]);

	// Navigating to a different artifact in this same pane starts the D-08
	// chrome variant over — a stale "Version history" or "Open source" split
	// from the file that used to be here would be confusing, not a feature.
	// Zoom/device intentionally survive (closer to how a browser tab's zoom
	// persists across navigation).
	useEffect(() => {
		setVariant(stateKey, 'default');
	}, [path, stateKey, setVariant]);

	// The viewer leaves this pane (tab switched away, pane closed): drop its
	// chrome state so the store doesn't keep an entry per pane for the whole
	// session and the next artifact here starts from a clean slate.
	useEffect(() => {
		return () => resetViewerState(stateKey);
	}, [stateKey, resetViewerState]);

	// Popped out into its own window — render the reclaim placeholder, not the
	// live duplicate.
	if (isDetached) {
		return <DetachedSurfacePlaceholder surfaceId={surfaceId} noun="file" />;
	}

	let content: React.ReactNode;
	if (stopped) {
		content = <ArtifactStoppedPlate path={path} onRestart={restart} />;
	} else if (variant === 'history') {
		content = (
			<VersionHistoryPanel path={path} onClose={() => setVariant(stateKey, 'default')} />
		);
	} else if (variant === 'source') {
		content = (
			<div className="grid h-full min-h-0 grid-cols-2 divide-x divide-border">
				<DeviceZoomFrame device={device} zoom={zoom}>
					<ViewerRouter key={reloadKey} path={path} source="pane" paneId={paneId} chromeless editable />
				</DeviceZoomFrame>
				<Suspense fallback={<CodeViewLoading />}>
					<CodeView path={path} line={line} col={col} />
				</Suspense>
			</div>
		);
	} else {
		content = (
			<DeviceZoomFrame device={device} zoom={zoom}>
				<ViewerRouter
					key={reloadKey}
					path={path}
					source="pane"
					paneId={paneId}
					chromeless
					editable
					line={line}
					col={col}
				/>
			</DeviceZoomFrame>
		);
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
			{stopped && !stoppedStripDismissed && (
				<ArtifactInfoStrip
					kind="stopped"
					onRestart={restart}
					onDismiss={() => setStoppedStripDismissed(true)}
				/>
			)}
			{!stopped && changed && variant === 'default' && (
				<ArtifactInfoStrip kind="changed" onDismiss={dismiss} />
			)}
			<div className="min-h-0 flex-1">{content}</div>
		</div>
	);
}

function CodeViewLoading() {
	return (
		<div className="flex h-full items-center justify-center text-xs text-muted-foreground">
			Loading source…
		</div>
	);
}

/** D-08 `artifact-device` — centres the renderer on a "sunken ground" at a
 *  fixed device width, with a caption reporting the preset + zoom, matching
 *  designs/pane-chrome.html?state=artifact-device. Zoom applies at every
 *  device width, not just `full` (D-08's `⋯` menu doesn't gate one on the
 *  other). */
function DeviceZoomFrame({
	device,
	zoom,
	children,
}: {
	device: '390' | '768' | 'full';
	zoom: number;
	children: React.ReactNode;
}) {
	const scale = zoom / 100;
	const inner = (
		<div
			style={{
				transform: `scale(${scale})`,
				transformOrigin: 'top left',
				width: `${(1 / scale) * 100}%`,
				height: `${(1 / scale) * 100}%`,
			}}
			className="h-full w-full"
		>
			{children}
		</div>
	);

	if (device === 'full') {
		return <div className="h-full w-full overflow-hidden">{inner}</div>;
	}

	const width = device === '390' ? 390 : 768;
	return (
		<div className="flex h-full w-full flex-col overflow-hidden bg-muted/20">
			{/* `height: '100%'` (not `min-h-[…]`) so the scaled child's own
			    percentage-based compensation (see `inner` above) has a definite
			    ancestor height to resolve against all the way up — a `min-h`/auto
			    height here would make that percentage resolve to 0. */}
			<div className="flex min-h-0 flex-1 items-start justify-center overflow-auto p-6">
				<div
					style={{ width, height: '100%' }}
					className="shrink-0 overflow-hidden rounded-md border border-border bg-background shadow-sm"
				>
					{inner}
				</div>
			</div>
			<div className="flex shrink-0 items-center justify-center gap-2 border-t border-border py-1 font-mono text-[10px] text-muted-foreground">
				<span>{width} px · device width preset</span>
				<span className="rounded bg-muted px-1.5 py-0.5">{zoom}%</span>
			</div>
		</div>
	);
}
