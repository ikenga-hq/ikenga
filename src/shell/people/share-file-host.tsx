import { useShellStore } from '@/lib/shell/shell-store';

import { useShareFileStore } from './share-file';
import { absoluteFilePath } from './file-link-model';
import { ShareKolaSheet } from './share-kola-sheet';

/** The invite sheet opened by a file's "Share…" action (WP-11). Renders nothing until asked. */
export function ShareFileHost() {
	const path = useShareFileStore((s) => s.path);
	const close = useShareFileStore((s) => s.close);
	const projectId = useShellStore((s) => s.activeProjectId) || 'default';
	const projectName = useShellStore(
		(s) => s.projects.find((p) => p.id === (s.activeProjectId || 'default'))?.display_name
	);
	const root = useShellStore((s) => s.activeProject?.root_path);
	if (path === null) return null;
	return (
		<ShareKolaSheet
			open
			onOpenChange={(o) => {
				if (!o) close();
			}}
			projectId={projectId}
			projectName={projectName ?? projectId}
			onIssued={() => {}}
			initialScope="artifact"
			initialArtifactPath={path}
			absolutePath={absoluteFilePath(root, path) ?? undefined}
		/>
	);
}
