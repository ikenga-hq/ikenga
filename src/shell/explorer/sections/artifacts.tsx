import { useCallback } from 'react';
import { FileCode } from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import { loadRecentArtifacts, type RecentArtifact } from '@/lib/shell/artifact-grid-recent-artifacts';
import type { ExplorerSectionContext } from '../section-registry';

export const artifactsContextMenu = [
	{ id: 'open-loupe', label: 'Open (loupe)', run: () => {} },
	{ id: 'open-studio', label: 'Open in Studio (grid)', run: () => {} },
	{ id: 'compare', label: 'Compare with…', run: () => {} },
	{ id: 'open-side', label: 'Open to the Side', run: () => {} },
	{ id: 'pin-sidebar', label: 'Pin to Sidebar…', run: () => {} },
	{ id: 'copy-uri', label: 'Copy ikenga:// URI', run: () => {} },
	{ id: 'reveal-files', label: 'Reveal in Files', run: () => {} },
	{ id: 'hand-to-chi', label: 'Hand to Chi', run: () => {} },
];

export function ArtifactsSection({ projectId }: ExplorerSectionContext) {
	const query = useQuery<RecentArtifact[]>({
		queryKey: ['explorer-artifacts', projectId],
		queryFn: async () => {
			try {
				return loadRecentArtifacts(projectId);
			} catch {
				return [];
			}
		},
		staleTime: 10_000,
	});

	const artifacts = query.data ?? [];

	const openArtifact = useCallback((path: string) => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'artifact', path });
	}, []);

	if (artifacts.length === 0) {
		return (
			<div className="p-4 text-center">
				<h3 className="text-sm font-semibold">Nothing built yet</h3>
				<p className="text-xs text-muted-foreground mt-1 mb-3">
					Artifacts are the .html your agents write. The first one appears here the moment it lands on disk.
				</p>
				<button
					type="button"
					className="text-xs bg-primary text-primary-foreground px-3 py-1.5 rounded hover:bg-primary/90 transition-colors"
					onClick={() => { /* stubbed: Ask a Chi to build one */ }}
				>
					Ask a Chi to build one
				</button>
			</div>
		);
	}

	return (
		<div className="py-1">
			{artifacts.map((art) => (
				<ListRow
					key={art.path}
					size="sm"
					onActivate={() => openArtifact(art.path)}
					title={art.path}
					className="w-full gap-1.5 px-2"
				>
					<FileCode className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
					<span className="flex-1 truncate text-xs">{art.path.split('/').pop()}</span>
				</ListRow>
			))}
		</div>
	);
}
