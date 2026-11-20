// The one Download control behind every file surface (WP-05): the Files row
// menu, the viewer pane's `⋯` menu, the PDF and unknown views and the
// "cannot preview" plate all call this, so the save path, the label and the
// failure toast are the same everywhere.

import { useCallback, useRef, useState } from 'react';
import { toast } from '@/lib/toast';
import { isBrowserHost } from '@/lib/transport';
import { downloadFile } from '@/lib/transport/fs-download';
import { basename } from '../lib/path';

/** Browser: "Download". Desktop: the control keeps its "Open in default app" label. */
export function downloadLabel(): string {
	return isBrowserHost() ? 'Download' : 'Open in default app';
}

/** Run the download for `path`; a failure is a toast, never a throw. Resolves true on success. */
export async function downloadWithFeedback(path: string): Promise<boolean> {
	try {
		await downloadFile(path);
		return true;
	} catch (e) {
		toast({
			label: `Could not ${isBrowserHost() ? 'download' : 'open'} ${basename(path)}: ${e instanceof Error ? e.message : String(e)}`,
			variant: 'error',
		});
		return false;
	}
}

/** `{ busy, run }` for a button: `run` ignores a second click while one is in flight. */
export function useDownloadFile(path: string): { busy: boolean; run: () => Promise<void> } {
	const [busy, setBusy] = useState(false);
	const inFlight = useRef(false);
	const run = useCallback(async () => {
		if (inFlight.current) return;
		inFlight.current = true;
		setBusy(true);
		try {
			await downloadWithFeedback(path);
		} finally {
			inFlight.current = false;
			setBusy(false);
		}
	}, [path]);
	return { busy, run };
}
