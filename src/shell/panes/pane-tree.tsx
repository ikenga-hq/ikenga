import { Fragment } from 'react';
import { Panel, PanelGroup, PanelResizeHandle } from 'react-resizable-panels';
import { type PaneNode, type SplitNode } from '@/lib/panes/types';
import { usePaneStore } from '@/lib/panes/pane-store';
import { Pane } from './pane';
import './panes.css';

function leafKey(node: PaneNode): string {
	if (node.type === 'leaf') return node.id;
	return `s:${node.children.map(leafKey).join('|')}`;
}

interface PaneTreeNodeProps {
	node: PaneNode;
	path: number[];
}

function PaneTreeNode({ node, path }: PaneTreeNodeProps) {
	if (node.type === 'leaf') {
		return <Pane leaf={node} />;
	}
	return <PaneSplit node={node} path={path} />;
}

function PaneSplit({ node, path }: { node: SplitNode; path: number[] }) {
	const setSplitSizes = usePaneStore((s) => s.setSplitSizes);
	// Key the PanelGroup by the structural fingerprint of its children. When
	// the structure changes (split/close), the key changes, react-resizable-
	// panels remounts with the fresh `defaultSize` props, sizes redistribute
	// correctly. Per-pane state (TanStack memory routers, terminal sessions)
	// survives via module-level caches in the view components.
	const groupKey = node.children.map(leafKey).join('|');

	return (
		<PanelGroup
			key={groupKey}
			direction={node.direction}
			onLayout={(sizes) => {
				// Only persist non-degenerate layouts (PanelGroup occasionally emits
				// [0, 0, ...] during teardown).
				if (sizes.some((s) => s > 0)) setSplitSizes(path, sizes);
			}}
			className="flex-1"
		>
			{node.children.map((child, i) => (
				<Fragment key={leafKey(child)}>
					{i > 0 && (
						<PanelResizeHandle
							className={
								node.direction === 'horizontal'
									? 'w-px bg-border data-[resize-handle-state=hover]:bg-primary/40 data-[resize-handle-state=drag]:bg-primary/60'
									: 'h-px bg-border data-[resize-handle-state=hover]:bg-primary/40 data-[resize-handle-state=drag]:bg-primary/60'
							}
							data-panel-resize-handle-enabled="true"
							// §4.4: 1px visual, ~8px hit area. `hitAreaMargins` is the
							// library's own hit-test padding — it never adds to the
							// rendered box (the handle stays `w-px`/`h-px`), it only
							// widens the invisible region pointermove hit-tests against.
							hitAreaMargins={{ fine: 4, coarse: 8 }}
						/>
					)}
					<Panel
						id={leafKey(child)}
						order={i}
						defaultSize={node.sizes[i]}
						minSize={10}
						className="pane-leaf-container"
					>
						<PaneTreeNode node={child} path={[...path, i]} />
					</Panel>
				</Fragment>
			))}
		</PanelGroup>
	);
}

export function PaneTree() {
	const root = usePaneStore((s) => s.root);
	return (
		<div className="flex h-full w-full">
			<PaneTreeNode node={root} path={[]} />
		</div>
	);
}
