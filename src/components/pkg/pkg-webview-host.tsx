// Host-side placeholder for a kernel-owned child webview.
//
// Unlike `PkgIframeHost`, this component does NOT render the pkg's surface.
// The actual native webview is owned by Tauri's window and floats *over* the
// React tree at the rect this placeholder measures. The component's only
// jobs are:
//
//   1. Measure its DOM rect (window-client coords).
//   2. Ask the kernel to mount a webview at that rect on first render.
//   3. Reposition the webview whenever the placeholder's rect changes
//      (ResizeObserver + window resize).
//   4. Destroy the webview on unmount.
//
// No AppBridge, no MCP UI host, no host.* dispatcher — this surface is
// remote-controlled by the pkg's MCP server (via the kernel's eval path
// which is not exposed to the FE). Navigation after mount is driven by
// `pkgWebviewNavigate`, also invoked by the MCP server, not by React.
//
// Strict-mode safety: an effect that resolves *after* its cleanup ran must
// destroy the orphan it created. We use the same `dropped` flag pattern
// `PkgIframeHost` uses.

import { listen, type UnlistenFn } from '@/lib/transport';
import { useEffect, useRef, useState } from 'react';

import {
	pkgWebviewCreate,
	pkgWebviewDestroy,
	pkgWebviewNavigate,
	pkgWebviewSetRect,
	type PkgWebviewRect,
} from '@/lib/tauri-cmd';

// Tauri event payload emitted by `Kernel::reload_pkg`. The FE only cares about
// `pkg_id` for the host filter.
interface PkgReloadedEvent {
	pkg_id: string;
	version: string;
	registries: string[];
}

interface PkgWebviewHostProps {
	pkgId: string;
	/** The same pane_id the shell uses for layout. Used as the stable key for
	 *  the kernel-side webview handle so re-mounts find their existing webview
	 *  if one survived a strict-mode double-effect. */
	paneId: string;
	/** Initial URL — manifest's `ui.routes[].source`. May be navigated to other
	 *  origins at runtime via the pkg's MCP server; the React host doesn't
	 *  re-issue create on URL change. */
	source: string;
	/** Optional cookie partition name. The kernel resolves a per-pkg, per-name
	 *  data store. Omit / null → "default" partition. */
	partition?: string | null;
}

/** Read a DOM rect in window-client coords, rounded to integers. Tauri's
 *  `Window::add_child` takes window-client coords; on a single-window Tauri
 *  app these match viewport coords, but we round to avoid sub-pixel surprises
 *  in the native compositor.
 *
 *  Returns null if the element isn't yet laid out (zero size before mount). */
function measureRect(el: HTMLElement): PkgWebviewRect | null {
	const r = el.getBoundingClientRect();
	if (r.width <= 0 || r.height <= 0) return null;
	return {
		x: Math.round(r.left),
		y: Math.round(r.top),
		w: Math.round(r.width),
		h: Math.round(r.height),
	};
}

function rectsEqual(a: PkgWebviewRect | null, b: PkgWebviewRect | null): boolean {
	if (!a || !b) return a === b;
	return a.x === b.x && a.y === b.y && a.w === b.w && a.h === b.h;
}

export function PkgWebviewHost({ pkgId, paneId, source, partition }: PkgWebviewHostProps) {
	const placeholderRef = useRef<HTMLDivElement>(null);
	const [error, setError] = useState<string | null>(null);
	const [mounted, setMounted] = useState(false);

	// Mount effect — create the webview at the placeholder's initial rect.
	useEffect(() => {
		const el = placeholderRef.current;
		if (!el) return;

		let dropped = false;
		// Track whether create() resolved before cleanup so we know if there's
		// a kernel-side webview to destroy.
		let created = false;

		(async () => {
			// Wait one frame so layout has settled before measuring. React's
			// commit phase runs effects after paint, but during route
			// transitions the parent panel may still be animating in.
			await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
			if (dropped) return;

			const rect = measureRect(el) ?? { x: 0, y: 0, w: 0, h: 0 };

			try {
				await pkgWebviewCreate(pkgId, paneId, source, rect, partition ?? null);
				if (dropped) {
					// Strict-mode double-effect: cleanup ran while create() was
					// in flight. Clean up the orphan we just created.
					pkgWebviewDestroy(pkgId, paneId).catch((e) => {
						console.warn(`[pkg-webview-host] orphan destroy failed for ${pkgId}/${paneId}:`, e);
					});
					return;
				}
				created = true;
				setMounted(true);
			} catch (e) {
				if (!dropped) {
					setError((e as Error).message ?? String(e));
				}
			}
		})();

		return () => {
			dropped = true;
			if (created) {
				pkgWebviewDestroy(pkgId, paneId).catch((e) => {
					console.warn(`[pkg-webview-host] destroy failed for ${pkgId}/${paneId}:`, e);
				});
			}
		};
		// `source` and `partition` are captured at mount only — the React side
		// doesn't re-create on URL changes (navigation goes through the pkg's
		// MCP server). pkgId / paneId changes effectively mean a different
		// webview, so re-create.
	}, [pkgId, paneId, source, partition]);

	// Dev-mode: `Kernel::reload_pkg` emits `pkg-reloaded` after re-registering.
	// For child webviews we deliberately do NOT destroy + re-create — that
	// would wipe the cookie partition mid-loop (the partition data persists
	// on disk, but the dev would still see logged-out sessions every save).
	// Instead, navigate to the current `source` URL, which refreshes the
	// page in place. If the manifest actually changed `ui.routes[].source`,
	// the parent route resolver will re-render with a new `source` prop and
	// the mount effect above will tear down + re-create normally.
	useEffect(() => {
		if (!mounted) return;
		let unlisten: UnlistenFn | null = null;
		let cancelled = false;
		listen<PkgReloadedEvent>('pkg-reloaded', (ev) => {
			if (cancelled) return;
			if (ev.payload?.pkg_id !== pkgId) return;
			pkgWebviewNavigate(pkgId, paneId, source).catch((e) => {
				console.warn(`[pkg-webview-host] navigate on reload failed for ${pkgId}/${paneId}:`, e);
			});
		}).then((fn) => {
			if (cancelled) {
				fn();
				return;
			}
			unlisten = fn;
		});
		return () => {
			cancelled = true;
			unlisten?.();
		};
	}, [mounted, pkgId, paneId, source]);

	// Reposition effect — observe placeholder + window resize, debounce via
	// rAF so a drag doesn't fire dozens of IPC calls per frame.
	useEffect(() => {
		if (!mounted) return;
		const el = placeholderRef.current;
		if (!el) return;

		let rafId: number | null = null;
		let lastRect: PkgWebviewRect | null = measureRect(el);
		let disposed = false;

		const flush = () => {
			rafId = null;
			if (disposed) return;
			const next = measureRect(el);
			if (!next) return;
			if (rectsEqual(next, lastRect)) return;
			lastRect = next;
			pkgWebviewSetRect(pkgId, paneId, next).catch((e) => {
				// Best-effort — the webview may have been destroyed in parallel
				// (rapid unmount during a drag). Don't surface as an error in
				// the UI; the next mount will re-create.
				console.warn(`[pkg-webview-host] set_rect failed for ${pkgId}/${paneId}:`, e);
			});
		};

		const schedule = () => {
			if (rafId !== null) return;
			rafId = requestAnimationFrame(flush);
		};

		const ro = new ResizeObserver(() => schedule());
		ro.observe(el);
		window.addEventListener('resize', schedule);

		return () => {
			disposed = true;
			if (rafId !== null) cancelAnimationFrame(rafId);
			ro.disconnect();
			window.removeEventListener('resize', schedule);
		};
	}, [mounted, pkgId, paneId]);

	if (error) {
		return (
			<div className="p-4 text-sm text-red-500">
				<div className="font-semibold">Failed to mount package webview</div>
				<div className="text-xs opacity-80 mt-1">{error}</div>
			</div>
		);
	}

	// The placeholder div fills its parent and is intentionally empty. The
	// native webview floats above the React tree at this rect — putting any
	// non-transparent child here would visually conflict with the webview
	// during the brief "Mounting browser…" window AND could steal pointer
	// events on platforms where the native webview doesn't fully cover the
	// placeholder during animations.
	return (
		<div
			ref={placeholderRef}
			data-pkg-webview-host={pkgId}
			data-pkg-pane-id={paneId}
			style={{ position: 'relative', width: '100%', height: '100%' }}
		>
			{!mounted && (
				<div className="p-4 text-xs opacity-60" style={{ pointerEvents: 'none' }}>
					Mounting browser…
				</div>
			)}
		</div>
	);
}
