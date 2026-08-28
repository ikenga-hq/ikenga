import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { IKENGA_BRIDGE_VERSION, IKENGA_HOST_MSG, wrapChildMessage } from './bridge-messages';
import {
	findArtifactFrames,
	requestFrameCapture,
	substituteArtifactFrames,
} from './capture-frames';

// jsdom has no HTMLImageElement.decode and never loads a src, so the real
// decode() would reject and the substitution would silently no-op — which is
// exactly the failure mode these tests exist to catch.
beforeEach(() => {
	HTMLImageElement.prototype.decode = function decode() {
		return Promise.resolve();
	};
});

afterEach(() => {
	document.body.innerHTML = '';
	vi.restoreAllMocks();
	vi.useRealTimers();
});

const PNG = 'iVBORw0KGgoAAAANSUhEUg==';

/**
 * An artifact iframe whose child answers `capture` however `reply` says.
 * `null` reply means the child never answers at all.
 */
function mountFrame(reply: 'ok' | 'error' | 'silent'): HTMLIFrameElement {
	const iframe = document.createElement('iframe');
	iframe.setAttribute('data-artifact-frame', 'true');
	document.body.appendChild(iframe);

	const fakeWindow = {
		postMessage(msg: { data?: { kind?: string; requestId?: string } }) {
			if (msg?.data?.kind !== 'capture' || reply === 'silent') return;
			const requestId = msg.data.requestId as string;
			const payload =
				reply === 'ok'
					? { kind: 'capture-result' as const, requestId, base64: PNG, width: 10, height: 20 }
					: {
							kind: 'capture-result' as const,
							requestId,
							base64: '',
							width: 0,
							height: 0,
							error: 'boom',
						};
			// Deliver asynchronously, like a real postMessage.
			setTimeout(() => {
				window.dispatchEvent(
					new MessageEvent('message', {
						data: wrapChildMessage(payload),
						source: fakeWindow as never,
					})
				);
			}, 0);
		},
	};
	Object.defineProperty(iframe, 'contentWindow', { value: fakeWindow, configurable: true });
	return iframe;
}

describe('findArtifactFrames', () => {
	it('finds only frames marked as artifact frames', () => {
		mountFrame('ok');
		document.body.appendChild(document.createElement('iframe')); // unmarked
		expect(findArtifactFrames(document.body)).toHaveLength(1);
	});
});

describe('requestFrameCapture', () => {
	it('resolves a data URL when the child renders', async () => {
		await expect(requestFrameCapture(mountFrame('ok'))).resolves.toBe(
			`data:image/png;base64,${PNG}`
		);
	});

	it('resolves null when the child reports an error', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		await expect(requestFrameCapture(mountFrame('error'))).resolves.toBeNull();
	});

	it('resolves null when the frame has no contentWindow', async () => {
		const iframe = document.createElement('iframe');
		Object.defineProperty(iframe, 'contentWindow', { value: null, configurable: true });
		await expect(requestFrameCapture(iframe)).resolves.toBeNull();
	});

	it('ignores a reply carrying a different requestId', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		vi.useFakeTimers();
		const iframe = mountFrame('silent');
		const p = requestFrameCapture(iframe);
		// A well-formed capture-result from the right window, wrong request.
		window.dispatchEvent(
			new MessageEvent('message', {
				data: wrapChildMessage({
					kind: 'capture-result',
					requestId: 'not-ours',
					base64: PNG,
					width: 1,
					height: 1,
				}),
				source: iframe.contentWindow as never,
			})
		);
		await vi.advanceTimersByTimeAsync(3000);
		await expect(p).resolves.toBeNull();
	});

	it('ignores a well-formed reply from a window that is not this frame', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		vi.useFakeTimers();
		const iframe = mountFrame('silent');
		let sentId = '';
		(iframe.contentWindow as unknown as { postMessage: (m: never) => void }).postMessage = ((m: {
			data: { requestId: string };
		}) => {
			sentId = m.data.requestId;
		}) as never;
		const p = requestFrameCapture(iframe);
		window.dispatchEvent(
			new MessageEvent('message', {
				data: wrapChildMessage({
					kind: 'capture-result',
					requestId: sentId,
					base64: PNG,
					width: 1,
					height: 1,
				}),
				source: { impostor: true } as never,
			})
		);
		await vi.advanceTimersByTimeAsync(3000);
		await expect(p).resolves.toBeNull();
	});

	it('gives up rather than hanging when the child never answers', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		vi.useFakeTimers();
		const p = requestFrameCapture(mountFrame('silent'));
		await vi.advanceTimersByTimeAsync(3000);
		await expect(p).resolves.toBeNull();
	});

	it('rejects an unversioned envelope even from the right window', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		vi.useFakeTimers();
		const iframe = mountFrame('silent');
		let sentId = '';
		(iframe.contentWindow as unknown as { postMessage: (m: never) => void }).postMessage = ((m: {
			data: { requestId: string };
		}) => {
			sentId = m.data.requestId;
		}) as never;
		const p = requestFrameCapture(iframe);
		window.dispatchEvent(
			new MessageEvent('message', {
				// Correct marker and sender, missing version.
				data: {
					[IKENGA_HOST_MSG]: true,
					data: { kind: 'capture-result', requestId: sentId, base64: PNG },
				},
				source: iframe.contentWindow as never,
			})
		);
		await vi.advanceTimersByTimeAsync(3000);
		await expect(p).resolves.toBeNull();
		expect(IKENGA_BRIDGE_VERSION).toBe(1);
	});
});

describe('substituteArtifactFrames', () => {
	it('stands an <img> in for the frame, then restores exactly', async () => {
		const iframe = mountFrame('ok');
		const before = document.body.innerHTML;

		const restore = await substituteArtifactFrames(document.body);

		const img = document.body.querySelector('img');
		expect(img).not.toBeNull();
		expect(img?.getAttribute('src')).toBe(`data:image/png;base64,${PNG}`);
		expect(iframe.style.display).toBe('none');
		// The stand-in must precede the frame so it occupies its box.
		expect(img?.nextElementSibling).toBe(iframe);

		restore();
		expect(document.body.querySelector('img')).toBeNull();
		expect(document.body.innerHTML).toBe(before);
	});

	it('leaves a frame untouched when it fails to render', async () => {
		vi.spyOn(console, 'warn').mockImplementation(() => {});
		const iframe = mountFrame('error');
		const restore = await substituteArtifactFrames(document.body);

		expect(document.body.querySelector('img')).toBeNull();
		expect(iframe.style.display).not.toBe('none');
		restore();
	});

	it('is a no-op with no artifact frames', async () => {
		document.body.appendChild(document.createElement('iframe'));
		const before = document.body.innerHTML;
		const restore = await substituteArtifactFrames(document.body);
		restore();
		expect(document.body.innerHTML).toBe(before);
	});

	it('substitutes every frame in a multi-pane capture', async () => {
		mountFrame('ok');
		mountFrame('ok');
		const restore = await substituteArtifactFrames(document.body);
		expect(document.body.querySelectorAll('img')).toHaveLength(2);
		restore();
		expect(document.body.querySelectorAll('img')).toHaveLength(0);
	});
});
