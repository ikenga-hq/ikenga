// How the terminal pastes, per host.
//
// Desktop (Tauri): the paste keys read the clipboard through the Tauri
// clipboard plugin. `navigator.clipboard.readText()` is avoided there because
// WebView2 gates it behind a per-origin prompt, and a dismissed prompt made
// paste fail silently.
//
// Browser (a remote ikenga-server session): the paste keys must NOT be
// intercepted. The browser's own paste fires a `paste` event on xterm's hidden
// textarea, which xterm turns into `term.paste(text)` — no clipboard
// permission involved. Reading the clipboard ourselves needs a permission
// grant, isn't supported by Firefox, and used to fail silently, so Ctrl+V /
// Ctrl+Shift+V did nothing.
import { isTauri } from '@/lib/transport';

/** True when the paste keys should be left to the browser's native paste. */
export function pasteKeyIsNative(): boolean {
	return !isTauri();
}

/** Shown when a menu-driven clipboard read is refused (browsers only allow it
 *  with a permission grant; Firefox not at all). */
export function menuPasteBlockedHint(): string {
	return isTauri()
		? "Couldn't read the clipboard."
		: 'Your browser blocked paste from the menu — press Ctrl+Shift+V (or Ctrl+V) to paste.';
}
