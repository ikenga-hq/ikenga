import { useCallback } from 'react';
import { FileEdit } from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import { listScratchpads } from '@/lib/iyke/memory';
import type { ExplorerSectionContext } from '../section-registry';

export const scratchpadsContextMenu = [
	{ id: 'open', label: 'Open', run: () => {} },
	{ id: 'open-side', label: 'Open to the Side', run: () => {} },
	{ id: 'rename', label: 'Rename…', run: () => {} },
	{ id: 'delete', label: 'Delete', run: () => {} },
];

export function ScratchpadsSection({ projectId }: ExplorerSectionContext) {
	const scope = `project:${projectId}`;
	const query = useQuery({
		queryKey: ['explorer-scratchpads', projectId],
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
				<ListRow
					key={sp.name}
					size="sm"
					onActivate={() => openScratchpad(sp.name)}
					title={sp.name}
					className="w-full gap-1.5 px-2"
				>
					<FileEdit className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
					<span className="flex-1 truncate text-xs">{sp.name}</span>
				</ListRow>
			))}
		</div>
	);
}
