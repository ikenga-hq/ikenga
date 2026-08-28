// Shared postMessage protocol between the shell (host) and an Ikenga artifact
// iframe (child). This file is safe to import from both sides; the child bundle
// (`bun run artifact:bundle`) inlines it, and the parent shell imports it to
// post messages and validate responses.
//
// The child iframe is sandboxed with `allow-scripts` but *without*
// `allow-same-origin`, so its document has an opaque origin and cannot read
// `window.parent`. All host/child coordination must go through `postMessage`.

export const IKENGA_HOST_MSG = '__ikenga_host';

/**
 * Envelope version. Both halves of the bridge ship from the same commit — the
 * child bundle is rebuilt by `artifact:bundle` and *injected* by the viewer
 * server into every `/__viewer/*` response, so it is never embedded in a
 * stored artifact and cannot lag the host. Skew between our two halves is
 * therefore not what this guards.
 *
 * What it guards is everything else on the `message` bus. An artifact's own
 * code, an embedded widget, or any other frame can postMessage anything it
 * likes; a strict equality check on a version we control means only our exact
 * envelope shape is ever treated as a bridge message. Bump this when the verb
 * lists below change shape, and keep the check `===` — default-deny.
 */
export const IKENGA_BRIDGE_VERSION = 1;

/** Theme attributes the shell broadcasts and the child applies. */
export interface ThemePayload {
	mode: 'light' | 'dark';
	theme: string;
	density: string;
}

/** A DOM rect serialized for postMessage. */
export interface SerializedRect {
	top: number;
	left: number;
	width: number;
	height: number;
}

/** A single pin resolution result. */
export interface PinResolution {
	selector: string;
	found: boolean;
	rect: SerializedRect | null;
}

/** Data needed to open the host context menu after a right-click pick. */
export interface PickPayload {
	/** CSS selector round-trippable via querySelector. */
	selector: string;
	/** Center of the element, fraction of the child's scroll viewport. */
	positionX: number;
	positionY: number;
	/** Base64 PNG of the element. */
	screenshotBase64: string;
	screenshotWidth: number;
	screenshotHeight: number;
	/** Short tag/text summary for the picker preview. */
	elementLabel: string;
	/** Cursor position inside the child viewport at the time of the pick. */
	clientX: number;
	clientY: number;
}

/** Messages sent from the host to the child iframe. */
export type HostToChildMessage =
	| { kind: 'ping' }
	| { kind: 'theme'; payload: ThemePayload }
	| { kind: 'start-pick' }
	| { kind: 'stop-pick' }
	| { kind: 'start-comment' }
	| { kind: 'stop-comment' }
	| { kind: 'start-text-edit' }
	| { kind: 'stop-text-edit' }
	| { kind: 'resolve-pins'; requestId: string; selectors: string[] }
	| { kind: 'watch-pins'; selectors: string[] }
	| { kind: 'unwatch-pins' }
	| { kind: 'capture'; requestId: string; selector: string };

/** Messages sent from the child iframe to the host. */
export type ChildToHostMessage =
	| { kind: 'ready' }
	| { kind: 'pong' }
	| { kind: 'pick'; payload: PickPayload }
	| { kind: 'hover'; rect: SerializedRect | null }
	| { kind: 'comment-pick'; payload: PickPayload }
	| { kind: 'text-edit-pick'; selector: string; rect: SerializedRect; originalHtml: string }
	| { kind: 'pins'; requestId: string; results: PinResolution[] }
	| { kind: 'pin-update'; results: PinResolution[] }
	| {
			kind: 'capture-result';
			requestId: string;
			base64: string;
			width: number;
			height: number;
			error?: string;
	  }
	| { kind: 'text-edit-commit'; selector: string; innerHtml: string; originalHtml: string }
	| { kind: 'text-edit-cancel'; selector: string };

/**
 * Wrapper the child must use so the host can distinguish Ikenga messages.
 *
 * There is deliberately no `origin` field. An earlier revision carried
 * `origin: 'null'` described as an "origin guard", which it could not be: a
 * value the sender writes into its own payload is not a guard, since anything
 * posting to the bus can write it too. The real check is on the `MessageEvent`
 * — `e.source` — and both ends now do it (see `isFromExpectedSender`).
 */
export interface ChildMessageWrapper {
	[IKENGA_HOST_MSG]: true;
	v: number;
	data: ChildToHostMessage;
}

/** Wrapper the host must use so the child can distinguish Ikenga messages. */
export interface HostMessageWrapper {
	[IKENGA_HOST_MSG]: true;
	v: number;
	data: HostToChildMessage;
}

export function isIkengaHostMessage(
	data: unknown
): data is HostMessageWrapper | ChildMessageWrapper {
	if (typeof data !== 'object' || data === null) return false;
	const d = data as Record<string, unknown>;
	return (
		d[IKENGA_HOST_MSG] === true &&
		d.v === IKENGA_BRIDGE_VERSION &&
		typeof d.data === 'object' &&
		d.data !== null
	);
}

/**
 * Is this event from the window we expect to be talking to?
 *
 * `e.origin` cannot carry this. The child is sandboxed without
 * `allow-same-origin`, so every message it sends arrives with origin `"null"` —
 * and `"null"` is what *every* sandboxed frame reports, so matching on it
 * proves nothing about which frame sent it. Comparing the `WindowProxy` in
 * `e.source` against the specific window we mean is the check that holds:
 * a `WindowProxy` is not forgeable across a postMessage.
 *
 * Host side passes `iframe.contentWindow`; child side passes `window.parent`.
 */
export function isFromExpectedSender(e: MessageEvent, expected: Window | null): boolean {
	return expected !== null && e.source === expected;
}

export function wrapHostMessage(data: HostToChildMessage): HostMessageWrapper {
	return { [IKENGA_HOST_MSG]: true, v: IKENGA_BRIDGE_VERSION, data };
}

export function wrapChildMessage(data: ChildToHostMessage): ChildMessageWrapper {
	return { [IKENGA_HOST_MSG]: true, v: IKENGA_BRIDGE_VERSION, data };
}
