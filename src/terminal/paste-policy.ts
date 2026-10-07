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
import { isMacPlatform } from '@/lib/keymap/platform';
import { isTauri } from '@/lib/transport';
import { terminalKeyLabel } from './keybindings';

/** True when the paste keys should be left to the browser's native paste. */
export function pasteKeyIsNative(): boolean {
	return !isTauri();
}

/** Shown when a menu-driven clipboard read is refused or impossible (browsers
 *  only allow it with a permission grant, Firefox not at all, and on an
 *  insecure origin there is no clipboard API at all). The key text comes from
 *  the registry, so it follows the platform and the user's own rebinding —
 *  never a hard-coded Ctrl+V, which on macOS sends ^V to the shell. */
export function menuPasteBlockedHint(opts?: { mac?: boolean }): string {
	if (isTauri()) return "Couldn't read the clipboard.";
	const mac = opts?.mac ?? isMacPlatform();
	const key = terminalKeyLabel('paste', { mac });
	// Off macOS, plain Ctrl+V also pastes natively (xterm-host leaves it alone).
	const alt = !mac && key !== 'Ctrl+V' ? ' (or Ctrl+V)' : '';
	return `Your browser blocked paste from the menu — press ${key}${alt} to paste.`;
}
