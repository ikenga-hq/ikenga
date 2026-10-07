// The terminal's clipboard-adjacent behaviours, pulled out of xterm-host so
// they can be tested without standing up an xterm instance.

import { copyText } from '@/lib/clipboard';
import { toast } from '@/lib/toast';
import { writeClipboardText } from '@/lib/transport/shims';

/**
 * The terminal's copy key (`terminal.copy`). Returns what the xterm custom key
 * handler should return: `false` = swallow, `true` = let xterm send it to the
 * PTY (macOS ⌘C with nothing selected is how SIGINT is sent).
 *
 * On Windows/Linux the default is Ctrl+Shift+C, which Chrome treats as
 * "inspect element" unless the event is `preventDefault`-ed. macOS ⌘C is left
 * alone: the native menu accelerator owns it.
 */
export function handleTerminalCopyKey(
	e: KeyboardEvent,
	opts: { selection: string; mac: boolean; copy?: (text: string) => void }
): boolean {
	const copy = opts.copy ?? ((text: string) => void copyText(text));
	if (opts.selection) {
		if (!opts.mac) e.preventDefault();
		copy(opts.selection);
		return false;
	}
	if (opts.mac) return true;
	e.preventDefault();
	return false;
}

/**
 * OSC 52 write (`\x1b]52;<targets>;<base64>`): a program in the terminal asks
 * to put text on the system clipboard. Read queries (`?`) are ignored: letting
 * a PTY program read the clipboard is an exfiltration vector.
 *
 * The write happens with no user activation, so Firefox and Safari refuse it
 * (and an insecure origin has no clipboard API). When it fails, raise a toast
 * with a Copy button: the click is a real gesture, so it succeeds, and the text
 * isn't lost.
 */
export function handleOsc52(
	data: string,
	deps: { write?: (text: string) => Promise<void> } = {}
): void {
	const write = deps.write ?? writeClipboardText;
	const sep = data.indexOf(';');
	const payload = sep === -1 ? data : data.slice(sep + 1);
	if (!payload || payload === '?') return;
	let text: string;
	try {
		const bytes = Uint8Array.from(atob(payload), (c) => c.charCodeAt(0));
		text = new TextDecoder().decode(bytes);
	} catch {
		return; // malformed OSC 52
	}
	write(text).catch(() => {
		toast({
			label: 'A program in the terminal tried to copy text, but the browser blocked it.',
			variant: 'notice',
			action: { label: 'Copy', run: () => void copyText(text, { successLabel: 'Copied' }) },
		});
	});
}

/** Open a link the terminal detected, without handing the page to it. */
export function openTerminalUrl(url: string): void {
	window.open(url, '_blank', 'noopener,noreferrer');
}
