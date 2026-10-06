// Shared state for an "Open file" affordance (settings.json, actions.json, …).
//
// Two jobs (gap audit rank 14):
//   * `available` — false in a browser session, where the daemon serves no
//     open-with-OS command, so the caller hides the link instead of offering
//     a silent no-op.
//   * `error` — the desktop path can fail (no default editor, unreadable
//     path), and every call site used to swallow that with `.catch(() => {})`.
//     `run` keeps the message so the caller can show it.

import { useCallback, useState } from 'react';
import { canOpenFilesWithOs } from '@/lib/tauri-cmd';

export function openFileErrorMessage(err: unknown): string {
	const detail = err instanceof Error ? err.message : String(err);
	return `Could not open the file: ${detail}`;
}

export interface OpenFileAction {
	/** False in a browser session — hide the affordance. */
	available: boolean;
	/** The last failure, cleared on the next attempt. */
	error: string | null;
	/** Run `open`, recording a failure instead of throwing. */
	run: (open: () => Promise<unknown>) => Promise<void>;
}

export function useOpenFile(): OpenFileAction {
	const [error, setError] = useState<string | null>(null);
	const run = useCallback(async (open: () => Promise<unknown>) => {
		setError(null);
		try {
			await open();
		} catch (err) {
			setError(openFileErrorMessage(err));
		}
	}, []);
	return { available: canOpenFilesWithOs(), error, run };
}
