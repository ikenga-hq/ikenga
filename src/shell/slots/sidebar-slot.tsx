// WP-20 (frame slot skeleton): the left sidebar panel + its resize handle.
//
// Renders today's Sidebar verbatim, still as a direct `<Panel>` child of the
// workspace's `<PanelGroup>` (react-resizable-panels registers panels via
// context, not DOM adjacency, so this indirection is behavior-preserving).
// The panel is conditionally rendered by `workspace.tsx` (hidden entirely
// when the nav rail is collapsed) — that condition stays in workspace.tsx
// since it also changes the content pane's `defaultSize`. Owned by WP-07
// going forward.
import { Panel, PanelResizeHandle } from 'react-resizable-panels';
import { Sidebar } from '@/shell/sidebar';

export function SidebarSlot({ defaultSize }: { defaultSize: number }) {
	return (
		<>
			<Panel defaultSize={defaultSize} minSize={8} maxSize={30} collapsible collapsedSize={6}>
				<Sidebar />
			</Panel>
			<PanelResizeHandle data-panel-resize-handle-enabled="true" aria-label="Resize sidebar" />
		</>
	);
}
