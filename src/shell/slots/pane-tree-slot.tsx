// WP-20 (frame slot skeleton): the content pane / tab + pane tree.
//
// Renders today's ContentPane verbatim, still as a direct `<Panel>` child of
// the workspace's `<PanelGroup>` (see sidebar-slot.tsx for why the
// indirection is behavior-preserving). Owned by WP-03 going forward.
import { Panel } from 'react-resizable-panels';
import { ContentPane } from '@/shell/content-pane';

export function PaneTreeSlot({ defaultSize }: { defaultSize: number }) {
	return (
		<Panel defaultSize={defaultSize} minSize={40}>
			<ContentPane />
		</Panel>
	);
}
