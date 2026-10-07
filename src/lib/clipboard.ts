// The one place a Copy action goes through. It uses the transport shim (Tauri
// plugin on desktop; navigator.clipboard, then the execCommand fallback, in a
// browser) and tells the user when the copy did not happen — instead of the
// silent no-op (or a premature "Copied") that a bare `navigator.clipboard`
// call gives on an insecure origin.

import { writeClipboardText } from '@/lib/transport/shims';
import { toast } from '@/lib/toast';

export interface CopyTextOptions {
	/** When set, a toast confirms the copy. Omit it where the UI already shows its own "Copied". */
	successLabel?: string;
	/** Override the failure toast text. */
	failureLabel?: string;
}

export const COPY_FAILED_LABEL = "Couldn't copy to the clipboard.";

/**
 * Copy `text`. Resolves `true` once the write really succeeded, `false` after
 * raising an error toast. Never rejects, so callers can `void` it.
 */
export async function copyText(text: string, opts: CopyTextOptions = {}): Promise<boolean> {
	try {
		await writeClipboardText(text);
	} catch (e) {
		console.warn('[clipboard] copy failed', e);
		toast({ label: opts.failureLabel ?? COPY_FAILED_LABEL, variant: 'error' });
		return false;
	}
	if (opts.successLabel) toast({ label: opts.successLabel, variant: 'notice' });
	return true;
}
