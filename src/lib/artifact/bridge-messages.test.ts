import { describe, expect, it } from 'vitest';

import {
	IKENGA_BRIDGE_VERSION,
	IKENGA_HOST_MSG,
	isFromExpectedSender,
	isIkengaHostMessage,
	wrapChildMessage,
	wrapHostMessage,
} from './bridge-messages';

// The artifact iframe is sandboxed without `allow-same-origin`, so postMessage
// is the only channel and every message on it arrives with origin "null" —
// including messages from frames that are not ours. These two guards are the
// whole of the channel's authentication, so they are worth pinning.

describe('isIkengaHostMessage', () => {
	it('accepts envelopes produced by our own wrappers', () => {
		expect(isIkengaHostMessage(wrapHostMessage({ kind: 'ping' }))).toBe(true);
		expect(isIkengaHostMessage(wrapChildMessage({ kind: 'ready' }))).toBe(true);
	});

	it('rejects a foreign message that mimics the marker but not the version', () => {
		// The shape an attacker would reach for first: copy the marker key.
		expect(isIkengaHostMessage({ [IKENGA_HOST_MSG]: true, data: { kind: 'capture' } })).toBe(false);
		expect(
			isIkengaHostMessage({
				[IKENGA_HOST_MSG]: true,
				v: IKENGA_BRIDGE_VERSION + 1,
				data: { kind: 'capture' },
			})
		).toBe(false);
		// A string version must not coerce past a `===` check.
		expect(
			isIkengaHostMessage({
				[IKENGA_HOST_MSG]: true,
				v: String(IKENGA_BRIDGE_VERSION),
				data: { kind: 'capture' },
			})
		).toBe(false);
	});

	it('rejects junk without throwing', () => {
		for (const junk of [
			null,
			undefined,
			0,
			'',
			'ping',
			[],
			{ data: {} },
			{ [IKENGA_HOST_MSG]: true, v: 1 },
		]) {
			expect(isIkengaHostMessage(junk)).toBe(false);
		}
	});

	it('rejects a null `data` that would otherwise pass a typeof check', () => {
		// `typeof null === 'object'`, which is exactly how the previous
		// implementation would have let this through.
		expect(
			isIkengaHostMessage({ [IKENGA_HOST_MSG]: true, v: IKENGA_BRIDGE_VERSION, data: null })
		).toBe(false);
	});
});

describe('isFromExpectedSender', () => {
	const expected = { name: 'expected' } as unknown as Window;
	const other = { name: 'other' } as unknown as Window;

	it('accepts only the exact window we expect', () => {
		expect(isFromExpectedSender({ source: expected } as unknown as MessageEvent, expected)).toBe(
			true
		);
		expect(isFromExpectedSender({ source: other } as unknown as MessageEvent, expected)).toBe(
			false
		);
	});

	it('denies when the expected window is absent', () => {
		// A detached iframe has a null contentWindow. Without the null guard,
		// `e.source === null` from an unrelated sender would authenticate.
		expect(isFromExpectedSender({ source: null } as unknown as MessageEvent, null)).toBe(false);
	});

	it('denies a message carrying no source', () => {
		expect(isFromExpectedSender({} as unknown as MessageEvent, expected)).toBe(false);
	});
});

describe('envelope shape', () => {
	it('carries no self-declared origin field', () => {
		// A value the sender writes into its own payload cannot authenticate it;
		// an earlier revision shipped `origin: 'null'` described as a guard.
		expect(wrapChildMessage({ kind: 'ready' })).not.toHaveProperty('origin');
	});
});
