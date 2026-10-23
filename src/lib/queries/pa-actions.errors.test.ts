import { describe, expect, it } from 'vitest';
import { draftActionErrorMessage } from './pa-actions';

describe('draftActionErrorMessage', () => {
	it("turns the browser's network error into a sentence that says what to do", () => {
		for (const raw of [
			'Failed to fetch',
			'Load failed',
			'NetworkError when attempting to fetch resource.',
			'Network request failed',
		]) {
			const m = draftActionErrorMessage(new TypeError(raw));
			expect(m).toMatch(/Couldn't reach the box/);
			expect(m).toMatch(/reload the list/);
			expect(m).not.toMatch(/Failed to fetch/);
		}
	});

	it('explains an HTTP failure from the box', () => {
		expect(draftActionErrorMessage(new Error('HTTP RPC error: 502 Bad Gateway'))).toMatch(
			/did not answer properly \(HTTP 502\)/
		);
	});

	it('still shows the sentence of a share refusal, and a default for nothing', () => {
		expect(draftActionErrorMessage(new Error('approval_refused: The draft changed.'))).toBe(
			'The draft changed.'
		);
		expect(draftActionErrorMessage(undefined)).toBe('The action failed. Nothing was changed.');
	});
});
