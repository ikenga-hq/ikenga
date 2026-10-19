import { describe, expect, it } from 'vitest';
import { filterTreeViews, isOneShotRoute } from './pane-persistence';
import { addTab, getLeafIdsInOrder, leafCount, makeLeaf, splitLeaf } from './pane-reducer';
import type { LeafNode, PaneNode, PaneView, SplitNode } from './types';

function newRoute(path: string): LeafNode {
	return makeLeaf({ kind: 'route', path });
}

describe('filterTreeViews', () => {
	it('drops artifact tabs and keeps route tabs', () => {
		let root: PaneNode = newRoute('/inbox');
		const id = (root as LeafNode).id;
		root = addTab(root, id, { kind: 'artifact', path: '/note.html' });
		root = addTab(root, id, { kind: 'route', path: '/finance' });

		const live = new Set<string>();
		const r = filterTreeViews(
			root,
			(v) => v.kind === 'route' || (v.kind === 'terminal' && live.has(v.sessionId))
		);
		expect(r).not.toBeNull();
		const leaf = r as LeafNode;
		expect(leaf.tabs).toHaveLength(2);
		expect(leaf.tabs.every((t) => t.kind === 'route')).toBe(true);
	});

	it('drops terminal tabs whose session is gone, keeps live ones', () => {
		let root: PaneNode = newRoute('/');
		const id = (root as LeafNode).id;
		root = addTab(root, id, { kind: 'terminal', sessionId: 't-alive' });
		root = addTab(root, id, { kind: 'terminal', sessionId: 't-dead' });

		const live = new Set(['t-alive']);
		const keep = (v: PaneView) =>
			v.kind === 'route' ||
			v.kind === 'artifact' ||
			(v.kind === 'terminal' && live.has(v.sessionId));
		const r = filterTreeViews(root, keep) as LeafNode;
		expect(r.tabs).toHaveLength(2); // route + alive terminal
		expect(r.tabs.find((t) => t.kind === 'terminal' && t.sessionId === 't-dead')).toBeUndefined();
	});

	it('removes leaves that have no tabs left and collapses one-child splits', () => {
		let root: PaneNode = newRoute('/a');
		const r1 = splitLeaf(root, (root as LeafNode).id, 'horizontal');
		expect(r1.ok).toBe(true);
		root = r1.root;
		const ids = getLeafIdsInOrder(root);
		// Replace the right pane's tabs with artifact-only.
		root = (function setAllArtifact(node: PaneNode): PaneNode {
			if (node.type === 'leaf') {
				if (node.id !== ids[1]) return node;
				return {
					...node,
					tabs: [{ kind: 'artifact', path: '/c1.html' }],
					activeTabIdx: 0,
				};
			}
			return { ...node, children: node.children.map(setAllArtifact) };
		})(root);

		const r = filterTreeViews(root, (v) => v.kind !== 'artifact');
		expect(r).not.toBeNull();
		// Right pane's only tab was an artifact — drops the leaf, parent split
		// collapses to its single remaining child.
		expect(r!.type).toBe('leaf');
		expect(leafCount(r!)).toBe(1);
	});

	it('returns null when every leaf collapses', () => {
		let root: PaneNode = newRoute('/a');
		const r1 = splitLeaf(root, (root as LeafNode).id, 'horizontal');
		root = r1.root;
		// Force every tab to artifact.
		root = (function rewrite(node: PaneNode): PaneNode {
			if (node.type === 'leaf') {
				return {
					...node,
					tabs: [{ kind: 'artifact', path: `/${node.id}.html` }],
					activeTabIdx: 0,
				};
			}
			return { ...node, children: node.children.map(rewrite) };
		})(root);

		const r = filterTreeViews(root, (v) => v.kind !== 'artifact');
		expect(r).toBeNull();
	});

	it('clamps activeTabIdx when filtering removes the active tab', () => {
		let root: PaneNode = newRoute('/inbox');
		const id = (root as LeafNode).id;
		root = addTab(root, id, { kind: 'artifact', path: '/c1.html' }); // active becomes idx 1
		expect((root as LeafNode).activeTabIdx).toBe(1);

		const r = filterTreeViews(root, (v) => v.kind !== 'artifact') as LeafNode;
		expect(r.tabs).toHaveLength(1);
		expect(r.activeTabIdx).toBe(0);
	});

	it('redistributes split sizes after partial collapse', () => {
		let root: PaneNode = newRoute('/a');
		root = splitLeaf(root, (root as LeafNode).id, 'horizontal').root; // 2 leaves
		root = splitLeaf(root, getLeafIdsInOrder(root)[0], 'horizontal').root; // 3 leaves
		expect(leafCount(root)).toBe(3);

		const ids = getLeafIdsInOrder(root);
		// Mark middle leaf as artifact-only so it gets dropped.
		root = (function rewrite(node: PaneNode): PaneNode {
			if (node.type === 'leaf') {
				if (node.id !== ids[1]) return node;
				return {
					...node,
					tabs: [{ kind: 'artifact', path: '/c-mid.html' }],
					activeTabIdx: 0,
				};
			}
			return { ...node, children: node.children.map(rewrite) };
		})(root);

		const r = filterTreeViews(root, (v) => v.kind !== 'artifact');
		expect(r).not.toBeNull();
		expect(leafCount(r!)).toBe(2);
		expect(r!.type).toBe('split');
		expect((r as SplitNode).sizes).toEqual([50, 50]);
	});
});

describe('isOneShotRoute', () => {
	it('matches the single-use invite and pair pages only', () => {
		expect(isOneShotRoute({ kind: 'route', path: '/remote/invite' })).toBe(true);
		expect(isOneShotRoute({ kind: 'route', path: '/remote/pair' })).toBe(true);
		expect(isOneShotRoute({ kind: 'route', path: '/remote/invite/x' })).toBe(true);
		expect(isOneShotRoute({ kind: 'route', path: '/remote' })).toBe(false);
		expect(isOneShotRoute({ kind: 'route', path: '/remote/inviter' })).toBe(false);
		expect(isOneShotRoute({ kind: 'artifact', path: '/remote/invite' })).toBe(false);
	});

	it('a restored tree loses the invite tab and lands on the tab beside it', () => {
		let root: PaneNode = newRoute('/');
		const id = (root as LeafNode).id;
		root = addTab(root, id, { kind: 'route', path: '/remote/invite' });
		expect((root as LeafNode).activeTabIdx).toBe(1);
		const r = filterTreeViews(root, (v) => !isOneShotRoute(v)) as LeafNode;
		expect(r.tabs).toEqual([{ kind: 'route', path: '/' }]);
		expect(r.activeTabIdx).toBe(0);
	});
});
