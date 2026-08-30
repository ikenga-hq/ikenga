// Catch-all for installed-package iframe routes.
//
// Resolves /pkg/<pkgId>/<splat> against UiRoutesRegistry. If the registry
// has an entry for `pkg://<pkgId>/<splat>` with kind=iframe, we mount it via
// PkgIframeHost.
//
// STALE-SUBPATH HEALING: if the exact subpath isn't registered but the pkg IS
// installed (it has other registered routes), we redirect to the pkg's primary
// route (preferring `/`) instead of showing a hard error. This is what a
// persisted pane pointing at a since-removed subpath needs — e.g. a saved
// `/pkg/com.ikenga.tasks/tasks` pane after the tasks pkg moved to a single
// root route with in-iframe view switching. Only when the pkg has NO routes at
// all (genuinely uninstalled) do we render the NotFound 404 — that's the
// contract this mechanism exists to provide.
//
// (Historical note: Tasks used to be a host-route builtin under
// `src/routes/tasks/`; it's now a normal installed pkg, so the catch-all is the
// only path for it too.)

import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { useEffect, useState } from 'react';

import { ActionBar } from '@/components/pkg/actions/action-bar';
import { PkgIframeHost } from '@/components/pkg/pkg-iframe-host';
import { PkgWebviewHost } from '@/components/pkg/pkg-webview-host';
import { pkgKernelStatus } from '@/lib/tauri-cmd';
import { usePaneScope } from '@/shell/panes/views/route-view';

export const Route = createFileRoute('/pkg/$pkgId/$')({
	component: PkgRouteCatchAll,
});

interface UiRouteEntry {
	pkg_id: string;
	virtual_path: string;
	path: string;
	kind: string;
	source: string;
	partition?: string;
}

type State =
	| { kind: 'loading' }
	| { kind: 'iframe'; entry: UiRouteEntry }
	| { kind: 'webview'; entry: UiRouteEntry }
	| { kind: 'unmountable'; entry: UiRouteEntry }
	| { kind: 'redirect'; toSplat: string }
	| { kind: 'not_found'; routePath: string };

function PkgRouteCatchAll() {
	const { pkgId, _splat } = Route.useParams() as { pkgId: string; _splat: string };
	const navigate = useNavigate();
	const paneScope = usePaneScope();
	// The splat doesn't include the leading slash; the registry stores
	// entries with a leading-slash path, so we add it back here.
	const routePath = `/${_splat ?? ''}`.replace(/\/+$/, '/').replace(/\/$/, '') || '/';
	const paneId = paneScope ?? `${pkgId}:${routePath}`;

	const [state, setState] = useState<State>({ kind: 'loading' });

	useEffect(() => {
		let cancelled = false;
		(async () => {
			try {
				const status = await pkgKernelStatus();
				const reg = (status.registries.ui_routes ?? {}) as { entries?: UiRouteEntry[] };
				const entries = reg.entries ?? [];
				// Match on (pkg_id, path). Trailing-slash variants both supported
				// because manifests commonly declare `/foo` and `/foo/` as separate
				// routes pointing at different sources.
				const entry =
					entries.find((e) => e.pkg_id === pkgId && e.path === routePath) ??
					entries.find((e) => e.pkg_id === pkgId && e.path === routePath + '/');
				if (cancelled) return;
				if (!entry) {
					// No exact match. If the pkg is installed and has *some* route,
					// the subpath is stale (e.g. a persisted pane from a prior pkg
					// version) — heal it by redirecting to the pkg's primary route
					// (prefer `/`) rather than dead-ending on a 404.
					const pkgRoutes = entries.filter((e) => e.pkg_id === pkgId);
					const fallback =
						pkgRoutes.find((e) => e.path === '/') ??
						pkgRoutes.find((e) => e.path === '') ??
						pkgRoutes[0];
					if (fallback) {
						const fallbackSplat = fallback.path.replace(/^\//, '');
						// Guard against a self-redirect loop (target === current).
						if (fallbackSplat !== (_splat ?? '').replace(/\/+$/, '')) {
							setState({ kind: 'redirect', toSplat: fallbackSplat });
							return;
						}
					}
					setState({ kind: 'not_found', routePath });
					return;
				}
				if (entry.kind === 'iframe') {
					setState({ kind: 'iframe', entry });
				} else if (entry.kind === 'webview') {
					setState({ kind: 'webview', entry });
				} else {
					setState({ kind: 'unmountable', entry });
				}
			} catch (e) {
				if (!cancelled) {
					setState({
						kind: 'not_found',
						routePath: `${routePath} (resolve failed: ${(e as Error).message})`,
					});
				}
			}
		})();
		return () => {
			cancelled = true;
		};
	}, [pkgId, routePath, _splat]);

	// Perform the stale-subpath redirect once resolved. `replace` keeps the
	// broken URL out of history so Back doesn't bounce into it again.
	useEffect(() => {
		if (state.kind !== 'redirect') return;
		void navigate({
			to: '/pkg/$pkgId/$',
			params: { pkgId, _splat: state.toSplat },
			replace: true,
		});
	}, [state, navigate, pkgId]);

	if (state.kind === 'loading' || state.kind === 'redirect') {
		return <div className="p-6 text-sm opacity-60">Resolving package route…</div>;
	}
	if (state.kind === 'not_found') {
		return (
			<div className="p-6 text-sm">
				<div className="font-semibold mb-1">No such package route</div>
				<div className="opacity-70">
					<code>
						pkg://{pkgId}
						{state.routePath}
					</code>{' '}
					is not registered. The package may have been uninstalled, or this URL is stale.
				</div>
			</div>
		);
	}
	if (state.kind === 'unmountable') {
		return (
			<div className="p-6 text-sm">
				<div className="font-semibold mb-1">Package route is not mountable</div>
				<div className="opacity-70">
					<code>{state.entry.virtual_path}</code> declares
					<code> kind: "{state.entry.kind}"</code>. Only <code>kind: "iframe"</code> and{' '}
					<code>kind: "webview"</code> are supported for installable packages — component-kind
					routes are reserved for host-builtin marker installs.
				</div>
			</div>
		);
	}
	if (state.kind === 'webview') {
		return (
			<PkgWebviewHost
				pkgId={state.entry.pkg_id}
				paneId={paneId}
				source={state.entry.source}
				partition={state.entry.partition}
			/>
		);
	}
	// WP-25: a pane whose pkg `requires` skills surfaces those skills' actions
	// as a bar above the iframe (resolved via `list_skill_actions` → Ọba store).
	// ActionBar renders null for pkgs that contribute no actions; `empty:hidden`
	// collapses the padded strip entirely in that case.
	return (
		<div className="flex h-full flex-col">
			<div className="shrink-0 border-b border-border/60 px-3 py-2 empty:hidden">
				<ActionBar pkgId={state.entry.pkg_id} />
			</div>
			<div className="min-h-0 flex-1">
				<PkgIframeHost pkgId={state.entry.pkg_id} source={state.entry.source} />
			</div>
		</div>
	);
}
