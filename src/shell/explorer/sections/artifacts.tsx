import { useCallback, useState } from 'react';
import { FileCode, Layers } from 'lucide-react';
import { useQuery } from '@tanstack/react-query';
import { ListRow } from '@/components/ui/list-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import { loadRecentArtifacts, type RecentArtifact } from '@/lib/shell/artifact-grid-recent-artifacts';
import { usePathToManifestId } from '@/lib/shell/pins-store';
import { writeClipboardText } from '@/lib/transport';
import { handToChi } from '@/shell/companion/companion-store';
import { EmptyState } from '@/components/states';
import { EffectiveContextMenu } from '@/shell/menu/effective-context-menu';
import { PinArtifactDialog } from '@/shell/panes/pin-artifact-dialog';
import type { ExplorerSectionContext } from '../section-registry';

// WP-04 stub array — real menu content is `getEffectiveMenu('artifacts')`
// below (G-ACTIONS §1.3). Kept for `section-registry.ts`'s unused
// `contextMenu` field (out of this WP's FILES list; see the PR report).
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
	const pathToManifestId = usePathToManifestId();

	const openArtifact = useCallback((path: string) => {
		const { focusedId, addTab } = usePaneStore.getState();
		addTab(focusedId, { kind: 'artifact', path });
	}, []);

	const [pinPath, setPinPath] = useState<string | null>(null);

	if (artifacts.length === 0) {
		return (
			<EmptyState
				data-state="explorer-artifacts-empty"
				icon={Layers}
				heading="Nothing built yet"
				body="Artifacts are the .html your agents write. The first one appears here the moment it lands on disk."
				action={{
					label: 'Ask a Chi to build one',
					onClick: () => handToChi('Build me an artifact: '),
				}}
			/>
		);
	}

	return (
		<div className="py-1">
			{artifacts.map((art) => (
				<EffectiveContextMenu
					key={art.path}
					menuId="artifacts"
					handlers={{
						'open-loupe': () => {
							const { focusedId, addTab } = usePaneStore.getState();
							addTab(focusedId, { kind: 'artifact-studio', path: art.path, density: 'loupe' });
						},
						'open-studio': () => {
							const { focusedId, addTab } = usePaneStore.getState();
							addTab(focusedId, { kind: 'artifact-studio', path: art.path, density: 'grid' });
						},
						compare: () => {
							const { focusedId, addTab } = usePaneStore.getState();
							addTab(focusedId, { kind: 'artifact-studio', path: art.path, density: 'compare' });
						},
						'open-to-side': () => {
							const { focusedId, placeView } = usePaneStore.getState();
							placeView(focusedId, { kind: 'artifact', path: art.path }, 'right');
						},
						'pin-sidebar': () => setPinPath(art.path),
						'copy-uri': () => {
							const manifestId = pathToManifestId.get(art.path);
							const uri = manifestId ? `ikenga://artifact/${manifestId}` : art.path;
							void writeClipboardText(uri).catch(() => {});
						},
						'reveal-files': () => usePaneStore.getState().revealPath(art.path),
						'hand-to-chi': () => handToChi(art.path),
					}}
				>
					<ListRow
						size="sm"
						onActivate={() => openArtifact(art.path)}
						title={art.path}
						className="w-full gap-1.5 px-2"
					>
						<FileCode className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
						<span className="flex-1 truncate text-xs">{art.path.split('/').pop()}</span>
					</ListRow>
				</EffectiveContextMenu>
			))}
			{pinPath !== null && (
				<PinArtifactDialog
					open
					onOpenChange={(o) => {
						if (!o) setPinPath(null);
					}}
					path={pinPath}
					onPinned={() => setPinPath(null)}
				/>
			)}
		</div>
	);
}
