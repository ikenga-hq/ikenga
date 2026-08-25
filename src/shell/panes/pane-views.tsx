import type { PaneView } from '@/lib/panes/types';
import type { TerminalTitleResolver } from '@/terminal/use-terminal-titles';
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

/**
 * Tab label for a view.
 *
 * `resolveTerminal` is how a terminal tab gets a real name (`claude · shell`)
 * instead of the constant "Terminal" — it needs the session store plus the live
 * foreground poll, neither of which belongs in a pure function. Callers that
 * render tab strips pass `useTerminalTitles()`; everyone else omits it and gets
 * the old constant, which is still correct, just uninformative.
 */
export function viewLabel(view: PaneView, resolveTerminal?: TerminalTitleResolver): string {
	switch (view.kind) {
		case 'route': {
			const segs = view.path.split('/').filter(Boolean);
			if (segs.length === 0) return 'Dashboard';
			return segs[segs.length - 1].replace(/-/g, ' ');
		}
		case 'terminal':
			return resolveTerminal?.(view.sessionId)?.label ?? 'Terminal';
		case 'artifact': {
			const name = view.path.split('/').filter(Boolean).pop();
			return name ?? 'Artifact';
		}
		case 'artifact-studio': {
			const name = view.path.split('/').filter(Boolean).pop();
			const prefix = view.density === 'grid' ? 'Grid' : 'Studio';
			return `${prefix} · ${name ?? (view.density === 'grid' ? 'folder' : 'artifact')}`;
		}
		case 'scratchpad':
			return view.name;
	}
}

export function viewSubtitle(view: PaneView, resolveTerminal?: TerminalTitleResolver): string {
	switch (view.kind) {
		case 'route':
			return view.path || '/';
		case 'terminal':
			// The terminal tooltip is multi-line — label, full cwd, argv, agent
			// label — so the tab's hover carries everything the label had to drop.
			return resolveTerminal?.(view.sessionId)?.tooltip ?? `session: ${view.sessionId}`;
		case 'artifact':
			return view.path;
		case 'artifact-studio':
			return view.density === 'compare' && view.vs ? `${view.path} ↔ ${view.vs}` : view.path;
		case 'scratchpad':
			return view.scope;
	}
}
