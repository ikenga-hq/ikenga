// Pasting an image into a terminal in a browser session.
//
// The terminal only ever receives text. On desktop an image on the clipboard
// is handled (or ignored) by the OS-level path; in a browser the clipboard
// carries a file the page cannot turn into something the PTY can use without
// uploading it to the server, which is not built yet. Silence read as a broken
// paste, so say what is going on.

import { toast } from '@/lib/toast';
import { isBrowserHost } from '@/lib/transport';

export const IMAGE_PASTE_UNSUPPORTED_MESSAGE =
	'Pasting images into the terminal is not supported in the browser yet. Paste text, or use the desktop app.';

/** True when a paste carries a file or image and no text to paste instead. */
export function isImageOnlyPaste(data: DataTransfer | null | undefined): boolean {
	if (!data) return false;
	const hasFile =
		(data.files?.length ?? 0) > 0 || Array.from(data.items ?? []).some((i) => i.kind === 'file');
	if (!hasFile) return false;
	return !data.getData('text/plain');
}

export function notifyImagePasteUnsupported(): void {
	toast({ label: IMAGE_PASTE_UNSUPPORTED_MESSAGE, variant: 'info' });
}

/**
 * Called after a programmatic clipboard read came back empty: if the clipboard
 * holds an image, explain why nothing was pasted. Browser sessions only.
 */
export async function explainEmptyPaste(): Promise<void> {
	if (!isBrowserHost()) return;
	try {
		const items = await navigator.clipboard.read();
		if (items.some((item) => item.types.some((t) => t.startsWith('image/')))) {
			notifyImagePasteUnsupported();
		}
	} catch {
		// Clipboard read denied or unsupported: nothing to add.
	}
}

/** `paste` event handler for the terminal wrapper (browser sessions). */
export function onTerminalPasteEvent(e: ClipboardEvent): void {
	if (!isBrowserHost()) return;
	if (!isImageOnlyPaste(e.clipboardData)) return;
	e.preventDefault();
	e.stopPropagation();
	notifyImagePasteUnsupported();
}
