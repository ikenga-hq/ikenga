// WP-69 — the label of one tab in a multi-surface detached window (D-09
// `popout`: "Window 2 holds two tabs; clicking one switches").
//
// Lazy-loaded by the thin root, and only when the window holds more than one
// surface, so a single pop-out never parses the terminal-title poll.
// A terminal tab names itself off the Rust descriptor by PTY id, the same
// `claude · shell` label its own header shows; any other surface shows the
// tail of its id (a viewer's file name).

import { useTerminalTitleByPtyId } from '@/terminal/use-terminal-titles';

import { fallbackTabLabel, surfaceSuffix } from '../tab-label';

export default function SurfaceTabLabel({ surfaceId }: { surfaceId: string }) {
	const ptyId = surfaceId.startsWith('terminal:') ? surfaceSuffix(surfaceId) : null;
	const title = useTerminalTitleByPtyId(ptyId);
	return (
		<span className="truncate" title={title?.tooltip ?? surfaceId}>
			{title?.label ?? fallbackTabLabel(surfaceId)}
		</span>
	);
}
