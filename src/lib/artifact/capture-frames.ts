// Host half of WP-09: get artifact pixels into a screenshot taken by the shell.
//
// The artifact iframe is sandboxed without `allow-same-origin`, so
// `modern-screenshot` cannot walk into its document and `isUnwalkableIframe`
// drops it from the clone. Dropping is right for a genuinely foreign frame —
// its content could never reach the PNG — but an Ikenga artifact frame is
// running our own bridge and can render itself. So we ask it to, and stand the
// result in for the frame while the capture runs.
//
// The child half (`capture` → `capture-result`) already existed; nothing on the
// host ever sent the request, which is why artifact panes came out blank on the
// FE screenshot path while the native crop path kept working — the same pane
// looking fine or empty depending on which route the capture took.

import * as M from './bridge-messages';

/**
 * How long to wait for a frame to render itself.
 *
 * Deliberately well under `FE_CAPTURE_TIMEOUT_MS` in `screenshot.ts`: a frame
 * that is wedged must fall back to the old drop-it behaviour with budget left
 * for the outer capture, rather than taking the whole screenshot down with it.
 */
const FRAME_CAPTURE_TIMEOUT_MS = 2500;

/** Every artifact frame under `root`, walkable or not. */
export function findArtifactFrames(root: HTMLElement): HTMLIFrameElement[] {
	return Array.from(root.querySelectorAll<HTMLIFrameElement>('iframe[data-artifact-frame="true"]'));
}

/**
 * Ask one artifact frame to render its own document to a PNG data URL.
 *
 * Resolves `null` on any failure — no contentWindow, child reports an error,
 * or the child never answers. `null` means "capture without it", which is the
 * behaviour we had before this existed, so a broken frame degrades to the old
 * blank box instead of failing the screenshot.
 */
export function requestFrameCapture(iframe: HTMLIFrameElement): Promise<string | null> {
	const cw = iframe.contentWindow;
	if (!cw) return Promise.resolve(null);

	const requestId = `cap-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;

	return new Promise((resolve) => {
		let settled = false;
		const finish = (value: string | null) => {
			if (settled) return;
			settled = true;
			window.removeEventListener('message', onMessage);
			clearTimeout(timer);
			resolve(value);
		};

		const onMessage = (e: MessageEvent) => {
			if (!M.isIkengaHostMessage(e.data)) return;
			if (!M.isFromExpectedSender(e, cw)) return;
			const m = (e.data as M.ChildMessageWrapper).data;
			// Correlate on requestId: several frames can be in flight at once
			// during a window capture, all posting to this same listener.
			if (m.kind !== 'capture-result' || m.requestId !== requestId) return;
			if (m.error || !m.base64) {
				console.warn('[screenshot] artifact frame declined to render', m.error ?? '(empty)');
				finish(null);
				return;
			}
			finish(`data:image/png;base64,${m.base64}`);
		};

		const timer = setTimeout(() => {
			console.warn(
				`[screenshot] artifact frame did not render within ${FRAME_CAPTURE_TIMEOUT_MS}ms`
			);
			finish(null);
		}, FRAME_CAPTURE_TIMEOUT_MS);

		window.addEventListener('message', onMessage);
		// `'html'` rather than `'body'`: body can be shorter than the document
		// when the artifact sets its own height on the root element.
		cw.postMessage(M.wrapHostMessage({ kind: 'capture', requestId, selector: 'html' }), '*');
	});
}

/**
 * Swap every artifact frame under `root` for an `<img>` of its own rendering,
 * so the synchronous clone sees real pixels. Returns a restore function that
 * must run before the next paint the user sees — callers put it in a `finally`.
 *
 * Frames that fail to render are left exactly as they were, so they follow the
 * existing unwalkable-iframe path.
 */
export async function substituteArtifactFrames(root: HTMLElement): Promise<() => void> {
	const frames = findArtifactFrames(root);
	if (frames.length === 0) return () => {};

	const undo: Array<() => void> = [];

	// Fire all captures concurrently — a window capture can hold several
	// artifact panes, and serialising would multiply the timeout budget.
	const shots = await Promise.all(frames.map((f) => requestFrameCapture(f)));

	await Promise.all(
		frames.map(async (iframe, i) => {
			const dataUrl = shots[i];
			if (!dataUrl) return;

			const rect = iframe.getBoundingClientRect();
			const img = document.createElement('img');
			img.src = dataUrl;
			img.style.width = `${rect.width}px`;
			img.style.height = `${rect.height}px`;
			img.style.objectFit = 'fill';
			img.style.display = 'block';

			// The clone is synchronous, so an undecoded <img> would render as a
			// blank box — precisely the bug we are fixing.
			try {
				await img.decode();
			} catch {
				return;
			}

			const prevDisplay = iframe.style.display;
			iframe.parentNode?.insertBefore(img, iframe);
			iframe.style.display = 'none';

			undo.push(() => {
				img.remove();
				if (prevDisplay) {
					iframe.style.display = prevDisplay;
					return;
				}
				// The frame had no inline display. Assigning '' back would leave a
				// residual `style=""` attribute on every screenshotted frame, so
				// remove the property — and the attribute, if it is now empty.
				iframe.style.removeProperty('display');
				if (iframe.getAttribute('style') === '') iframe.removeAttribute('style');
			});
		})
	);

	return () => {
		for (const fn of undo) fn();
	};
}
