// Thin detached root (plans/multi-window WP-05).
//
// The detached counterpart to `shell/workspace.tsx`: it mounts ONLY the
// surface(s) named in this window's `surface_set`, with none of the
// activity-bar / sidebar / pane-group chrome (pulling that in would defeat
// the perf goal — G-02). The `window://` lifecycle bus is subscribed once
// here at the substrate level and threaded into every surface, so surfaces
// stay in sync over the shared Rust core rather than mirroring its state.
//
// WP-69 (G-SEATS §4.4, DEC-69d — D-09 `popout`): a Pop out joins "Window 2"
// when one is open, so this window can hold SEVERAL surfaces. With more than
// one it shows a tab strip ("Window 2 holds two tabs; clicking one
// switches"); with one the strip merges away, as D-09 draws. Only the active
// tab is mounted: a hidden xterm would fit to a zero-size box and resize the
// shared PTY under the other window. The window's ⋯ (Move back to main
// window · Close Window 2) sits at the end of the active surface's header;
// minimise / restore / close are the OS window's own chrome.

import { lazy, Suspense } from 'react';

import { Tab, TabStrip } from '@/components/ui/tab-strip';
import type { WindowContext } from '@/lib/window/window-context';
import { registeredSurfaceIds, resolveSurface } from './registry';
import { fallbackTabLabel } from './tab-label';
import { useWindowLifecycle } from './use-window-lifecycle';
import { useWindowSurfaces } from './use-window-surfaces';

// Lazy: the Radix menu and the terminal-title poll stay out of first paint.
const WindowActions = lazy(() => import('./window-actions'));
const SurfaceTabLabel = lazy(() => import('./surfaces/surface-tab-label'));

function SurfaceFallback() {
	return (
		<div
			role="status"
			aria-live="polite"
			className="flex h-full w-full items-center justify-center text-muted-foreground text-sm"
		>
			Loading surface…
		</div>
	);
}

function UnknownSurface({ id }: { id: string }) {
	return (
		<div className="flex h-full w-full flex-col items-center justify-center gap-1 p-6 text-center text-sm">
			<p className="text-foreground">Unknown surface “{id}”.</p>
			<p className="text-muted-foreground">
				Registered: {registeredSurfaceIds().join(', ') || '—'}
			</p>
		</div>
	);
}

export function DetachedRoot({ ctx }: { ctx: WindowContext }) {
	// One subscription for the whole window — the cross-window event seam.
	const lifecycle = useWindowLifecycle();
	const { surfaces, active, setActive } = useWindowSurfaces(ctx);

	// Fall back to the placeholder when the URL named nothing, so the thin
	// path is always reachable/observable (and a bare `?window=detached-1`
	// still boots). A window whose last tab just left renders nothing: Rust
	// is closing it.
	const bare = ctx.surfaces.length === 0 && surfaces.length === 0;
	const surfaceIds = bare ? ['placeholder'] : surfaces;
	const activeId = bare ? 'placeholder' : (active ?? surfaceIds[0] ?? null);
	const activeIdx = activeId ? surfaceIds.indexOf(activeId) : -1;
	const tabbed = surfaceIds.length > 1;

	const actions = bare ? null : (
		<Suspense fallback={null}>
			<WindowActions label={ctx.label} surfaceId={activeId} />
		</Suspense>
	);

	const surface = activeId ? resolveSurface(activeId) : undefined;
	const Body = surface?.component;

	return (
		<div
			data-detached-window={ctx.label}
			data-state={tabbed ? 'window-tabs' : 'window-single'}
			className="flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground"
		>
			{tabbed && (
				<div className="flex h-8 shrink-0 items-stretch border-b border-border bg-card">
					<TabStrip
						label="Window tabs"
						className="flex-1"
						activeIdx={activeIdx}
						count={surfaceIds.length}
						onSwitch={(i) => {
							const id = surfaceIds[i];
							if (id) setActive(id);
						}}
					>
						{surfaceIds.map((id, i) => (
							<Tab
								key={id}
								index={i}
								active={id === activeId}
								data-window-tab={id}
								label={
									<Suspense fallback={<span className="truncate">{fallbackTabLabel(id)}</span>}>
										<SurfaceTabLabel surfaceId={id} />
									</Suspense>
								}
								title={id}
								onActivate={() => setActive(id)}
								className="min-w-[120px] max-w-[180px] border-r border-border px-3"
							/>
						))}
					</TabStrip>
				</div>
			)}
			<div className="flex min-h-0 flex-1 flex-col">
				{activeId && !surface && <UnknownSurface id={activeId} />}
				{activeId && surface && Body && (
					<>
						{!surface.ownsActions && actions && (
							<div className="flex h-8 shrink-0 items-center justify-end border-b border-border bg-muted/20 px-2">
								{actions}
							</div>
						)}
						<div className="min-h-0 flex-1">
							<Suspense key={activeId} fallback={<SurfaceFallback />}>
								<Body
									ctx={{ ...ctx, surfaces: [activeId] }}
									lifecycle={lifecycle}
									actions={surface.ownsActions ? (actions ?? undefined) : undefined}
								/>
							</Suspense>
						</div>
					</>
				)}
			</div>
		</div>
	);
}
