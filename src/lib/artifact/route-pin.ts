// Pin routing wrapper: `commentRoute` + the clipboard write it may ask for.
//
// The Rust dispatcher deliberately never touches the clipboard — a clipboard
// write has to happen inside the user's gesture to be permitted, and Rust
// can't guarantee that. So `comment_route` returns the rendered prompt in
// `clipboardText` and this helper performs the write. Every pin call site
// should go through here rather than calling `commentRoute` directly, so the
// clipboard sink can't be silently dropped again.

import { writeClipboardText } from '@/lib/transport';

import { commentRoute, type RouteResult, type RouteSink } from '@/lib/tauri-cmd';

export interface RoutePinOutcome extends RouteResult {
	/** True when `clipboardText` was successfully written to the clipboard.
	 *  False when the sink asked for a copy but the write failed — callers
	 *  should surface that rather than claim success. */
	copied: boolean;
}

/** Route a pin and satisfy the clipboard sink when the dispatcher picks it. */
export async function routePin(args: {
	id: number;
	overrideSink?: RouteSink;
	preferredPtyId?: string | null;
}): Promise<RoutePinOutcome> {
	const res = await commentRoute(args);
	if (!res.clipboardText) return { ...res, copied: false };
	try {
		await writeClipboardText(res.clipboardText);
		return { ...res, copied: true };
	} catch (e) {
		console.warn('[pin] clipboard write failed', e);
		return { ...res, copied: false };
	}
}

/** Short human label for a completed route, for toasts and status chips. */
export function routeOutcomeLabel(res: RoutePinOutcome): string {
	switch (res.sink) {
		case 'terminal':
			return res.ptyForeground ? `sent to ${res.ptyForeground}` : 'sent to terminal';
		case 'chi':
			return res.runId ? `chi run ${res.runId.slice(0, 8)}` : 'chi run started';
		case 'clipboard':
			return res.copied ? 'copied to clipboard' : 'copy failed';
		default:
			return 'not routed';
	}
}
