// Singleton layer that owns every pooled pkg iframe.
//
// Mounted ONCE near the workspace root (see workspace.tsx) — deliberately
// outside the pane tree and outside the PanelGroup remount scope — so the
// `<iframe>` elements it renders are never unmounted by a tab switch, reorder,
// split, or pane-tree rebuild. Each pooled iframe is a `<PkgIframeHostInner>`
// (the real bridge-owning renderer) inside a `position:fixed` box floated over
// the rect its in-pane placeholder (`PkgIframeSurface`) reports through the
// pool store. See src/lib/panes/iframe-pool.ts for the surface model.
//
// The layer only positions + toggles visibility; all the pkg lifecycle
// (AppBridge, host-context re-emits, dev-reload remount, token revoke on
// teardown) stays inside `PkgIframeHostInner`, which stays mounted for the
// surface's whole life and unmounts (full teardown) only on eviction.
//
// WebKitGTK stacking notes:
//   • z-index 30 sits ABOVE pane content but BELOW every overlay in the app
//     (dialogs / popovers / dropdowns / tooltips / sheets / command palette are
//     all z-50; floating toasts z-40). A dialog opened over a pane correctly
//     covers the pooled iframe.
//   • Hidden (released) surfaces use `visibility:hidden` (NOT `display:none`)
//     + `pointer-events:none`. visibility keeps the srcdoc iframe's compositing
//     layer and JS context continuously alive; a display:none→block toggle can
//     make WebKitGTK reload `about:srcdoc` (the Tauri #12767 class of bug),
//     which would defeat the entire keep-alive.
//   • During an active tab drag every pooled iframe is hidden the same way, so
//     the pointer-drag hit-test reaches the pane's own drop zones (z-20,
//     in-pane) and their hover indicator is visible — no z-index fight with a
//     fixed element.

import { useEffect } from 'react';

import { useDragState } from '@/lib/panes/drag-state';
import { type PoolSurface, useIframePool } from '@/lib/panes/iframe-pool';
import { usePaneStore } from '@/lib/panes/pane-store';
import { PkgIframeHostInner } from './pkg-iframe-host';

// Above pane content, below every overlay (all z-50; toasts z-40).
const POOL_Z_INDEX = 30;

function PooledIframe({ surface, hidden }: { surface: PoolSurface; hidden: boolean }) {
	const { rect, pkgId, source, paneId, key, refreshTick } = surface;
	return (
		<div
			data-pool-pane-id={paneId}
			data-pool-key={key}
			style={{
				position: 'fixed',
				left: rect ? rect.x : 0,
				top: rect ? rect.y : 0,
				width: rect ? rect.w : 0,
				height: rect ? rect.h : 0,
				zIndex: POOL_Z_INDEX,
				overflow: 'hidden',
				visibility: hidden ? 'hidden' : 'visible',
				pointerEvents: hidden ? 'none' : 'auto',
			}}
		>
			{/* refreshTick: the toolbar refresh button's reboot trigger for a
			    pooled surface — see iframe-pool.ts's `PoolSurface.refreshTick` and
			    pkg-iframe-host.tsx's Step-1 effect. Without threading it here the
			    surface's identity never changes on refresh, so the pooled iframe
			    would keep running its stale content indefinitely. */}
			<PkgIframeHostInner pkgId={pkgId} source={source} refreshTick={refreshTick} />
		</div>
	);
}

export function PkgIframeLayer() {
	const surfaces = useIframePool((s) => s.surfaces);
	// Hide every pooled iframe while a tab drag is in flight so the drag
	// controller's hit-test (`elementsFromPoint`) reaches the pane drop zones
	// beneath them instead of stopping at an iframe.
	const dragActive = useDragState((s) => s.active);

	// Focus routing. A pooled iframe is `position:fixed` outside the pane's DOM,
	// so clicking it won't bubble to pane.tsx's `onMouseDownCapture`. When the
	// main window blurs to one of our pooled iframes (same-origin srcdoc →
	// `document.activeElement` becomes that `<iframe>`), route focus to its
	// owning pane. Boring + observable; no ref sharing with the inner host.
	useEffect(() => {
		const onBlur = () => {
			requestAnimationFrame(() => {
				const el = document.activeElement as HTMLElement | null;
				if (!el || el.tagName !== 'IFRAME') return;
				const container = el.closest('[data-pool-pane-id]');
				const pid = container?.getAttribute('data-pool-pane-id');
				if (pid) usePaneStore.getState().focusPane(pid);
			});
		};
		window.addEventListener('blur', onBlur);
		return () => window.removeEventListener('blur', onBlur);
	}, []);

	return (
		<>
			{Object.values(surfaces).map((s) => (
				<PooledIframe key={s.key} surface={s} hidden={!s.claimed || dragActive} />
			))}
		</>
	);
}
