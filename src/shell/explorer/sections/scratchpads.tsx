import { useCallback } from 'react';
import { FileEdit } from 'lucide-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import { deleteScratchpad, listScratchpads } from '@/lib/iyke/memory';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import type { ExplorerSectionContext } from '../section-registry';

// WP-04 stub array — real menu content is `getEffectiveMenu('scratchpads')`
// below (G-ACTIONS §1.3). Kept for `section-registry.ts`'s unused
// `contextMenu` field (out of this WP's FILES list; see the PR report).
export const scratchpadsContextMenu = [
	{ id: 'open', label: 'Open', run: () => {} },
	{ id: 'open-side', label: 'Open to the Side', run: () => {} },
	{ id: 'rename', label: 'Rename…', run: () => {} },
	{ id: 'delete', label: 'Delete', run: () => {} },
];

export function ScratchpadsSection({ projectId }: ExplorerSectionContext) {
	const scope = `project:${projectId}`;
	const qc = useQueryClient();
	const queryKey = ['explorer-scratchpads', projectId];
	const query = useQuery({
		queryKey,
		queryFn: async () => {
			try {
				const res = await listScratchpads(scope);
				return res?.scratchpads ?? [];
			} catch {
				return [];
			}
		},
		staleTime: 15_000,
	});

	const items = query.data ?? [];

	const openScratchpad = useCallback((name: string) => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'scratchpad', scope, name });
	}, [scope]);

	const openScratchpadSplit = useCallback(
		(name: string) => {
			const { focusedId, placeView } = usePaneStore.getState();
			placeView(focusedId, { kind: 'scratchpad', scope, name }, 'right');
		},
		[scope]
	);

	const removeScratchpad = useCallback(
		async (name: string) => {
			const ok = await confirmDialog(`Delete scratchpad "${name}"?`, { title: 'Delete scratchpad', kind: 'warning' });
			if (!ok) return;
			try {
				await deleteScratchpad(name, scope);
			} finally {
				void qc.invalidateQueries({ queryKey });
			}
		},
		[scope, qc, queryKey]
	);

	const openScratchpadsPage = useCallback(() => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'route', path: '/scratchpads' });
	}, []);

	if (items.length === 0) {
		return (
			<div className="p-4 text-center">
				<h3 className="text-sm font-semibold">No scratchpads</h3>
				<p className="text-xs text-muted-foreground mt-1 mb-3">
					Scratchpads are project-scoped notes and scratch files.
				</p>
				<button
					type="button"
					onClick={openScratchpadsPage}
					className="text-xs bg-primary text-primary-foreground px-3 py-1.5 rounded hover:bg-primary/90 transition-colors"
				>
					New scratchpad
				</button>
			</div>
		);
	}

	return (
		<div className="py-1">
			{items.map((sp) => (
				<EffectiveContextMenu
					key={sp.name}
					menuId="scratchpads"
					// A-9: `rename` is left out — the memory API has no rename, so it
					// was read + write + delete (not atomic, and it could overwrite
					// an existing scratchpad of the new name) behind `window.prompt`.
					builtinsNeedHandler
					handlers={{
						open: () => openScratchpad(sp.name),
						'open-to-side': () => openScratchpadSplit(sp.name),
						delete: () => void removeScratchpad(sp.name),
					}}
				>
					<ListRow
						size="sm"
						onActivate={() => openScratchpad(sp.name)}
						title={sp.name}
						className="w-full gap-1.5 px-2"
					>
						<FileEdit className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
						<span className="flex-1 truncate text-xs">{sp.name}</span>
					</ListRow>
				</EffectiveContextMenu>
			))}
		</div>
	);
}
