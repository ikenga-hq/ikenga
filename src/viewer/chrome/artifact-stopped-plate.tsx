// D-08 `artifact-stopped` — full-content replacement when the viewer server
// is down (designs/pane-chrome.html?state=artifact-stopped, `stoppedPlate()`).
//
// Uses WP-43's `OfflineState` (src/components/states/) per the WP-44 brief
// ("loading/empty/error inside the viewer use WP-43's components"). Its type
// enforces exactly one `action` — the mock shows two buttons (Restart /
// "Open in default app"); the secondary one rides below the state component
// instead of inside its single-action prop.

import { CloudOff } from 'lucide-react';
import { OfflineState } from '@/components/states';
import { toast } from '@/lib/toast';
import { isBrowserHost, openLocalPath } from '@/lib/transport';

interface ArtifactStoppedPlateProps {
	path: string;
	onRestart: () => void;
}

export function ArtifactStoppedPlate({ path, onRestart }: ArtifactStoppedPlateProps) {
	return (
		<div className="flex h-full w-full flex-col items-center justify-center gap-3">
			<OfflineState
				data-state="artifact-stopped"
				icon={CloudOff}
				heading="Nothing is being served"
				body="The viewer's local HTTP server is not running, so this pane has nothing to render. The file itself is on disk and unchanged."
				action={{ label: 'Restart viewer server', onClick: onRestart }}
			/>
			<button
				type="button"
				className="text-xs text-muted-foreground underline-offset-2 hover:underline"
				// Matches `unknown-view.tsx`'s own "Open in default app". The OS
				// opens the path on desktop; a browser session downloads the file.
				onClick={() =>
					void openLocalPath(path, { kind: 'file' }).catch((e) =>
						toast({
							label: `Could not open the file: ${e instanceof Error ? e.message : String(e)}`,
							variant: 'error',
						})
					)
				}
			>
				{isBrowserHost() ? 'Download file' : 'Open in default app'}
			</button>
		</div>
	);
}
