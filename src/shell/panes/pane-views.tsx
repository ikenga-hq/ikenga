import { useQuery } from '@tanstack/react-query';
import { pkgKernelStatus } from '@/lib/tauri-cmd';
import type { PaneView } from '@/lib/panes/types';
import { RouteView } from './views/route-view';
import { TerminalView } from './views/terminal-view';
import { ArtifactView } from './views/artifact-view';
import { ArtifactStudioView } from './views/artifact-studio-view';
import { ScratchpadView } from './views/scratchpad-view';

interface PaneBodyProps {
	paneId: string;
	view: PaneView;
}

export function PaneBody({ paneId, view }: PaneBodyProps) {
	switch (view.kind) {
		case 'route':
			return <RouteView paneId={paneId} path={view.path} />;
		case 'terminal':
			return <TerminalView sessionId={view.sessionId} />;
		case 'artifact':
			return <ArtifactView path={view.path} paneId={paneId} line={view.line} col={view.col} />;
		case 'artifact-studio':
			return (
				<ArtifactStudioView
					path={view.path}
					paneId={paneId}
					density={view.density}
					vs={view.vs}
					attachedTerminalId={view.attachedTerminalId}
				/>
			);
		case 'scratchpad':
			return <ScratchpadView scope={view.scope} name={view.name} />;
	}
}

export { viewKey } from './view-key';

// Pure label/subtitle logic lives in `pane-view-label.ts`, free of the heavy
// view-component imports above — re-exported here so existing callers of
// `pane-views` keep working unchanged. Callers that only need label text
// (tab strip, address bar, ⌘K switcher) import `pane-view-label` directly
// instead, so computing a title doesn't drag in the whole view registry.
export {
	routeLabelInfo,
	shouldCapitalizeLabel,
	viewLabel,
	viewSubtitle,
	type RouteLabelInfo,
} from './pane-view-label';

/**
 * Resolves a PaneView to its webview UiRouteEntry if it's a webview route.
 * Used by PaneToolbar to inspect whether a pane is hosting a webview route.
 */
export function useWebviewRoute(view: PaneView | undefined) {
	const { data } = useQuery({
		queryKey: ['pkg-kernel-status'],
		queryFn: pkgKernelStatus,
		staleTime: Infinity,
	});

	if (!data || !view || view.kind !== 'route') return null;
	const match = view.path.match(/^\/pkg\/([^/]+)(.*)$/);
	if (!match) return null;
	const pkgId = match[1];
	const splat = match[2] || '/';

	const entries = (data.registries?.ui_routes as any)?.entries ?? [];
	const entry = entries.find(
		(e: any) => e.pkg_id === pkgId && (e.path === splat || e.path === splat + '/')
	);
	if (entry?.kind === 'webview') return entry;
	return null;
}
