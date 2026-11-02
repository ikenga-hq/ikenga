import { describe, expect, it } from 'vitest';
import {
	draftActionErrorMessage,
	paActionPatchFromArgs,
	paDraftSeenFromArgs,
	rejectReasonFromArgs,
} from './host-pa-actions';

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

describe('paActionPatchFromArgs (host.paActions.update patch whitelist)', () => {
	it('keeps every contract key of its declared type and drops the rest', () => {
		const r = paActionPatchFromArgs({
			subject: 'S',
			body: 'B',
			subjectB: 'SB',
			preheader: 'P',
			firstComment: 'FC',
			hashtags: ['#a', '#b'],
			media: { kind: 'carousel', imageUrls: ['https://x/1.png'], thread: [{ text: 'dropped' }] },
			scheduledIso: '2026-11-03T09:00:00+01:00',
			payloadJson: '{"item":{}}',
			status: 'committed',
		});
		expect(r).toEqual({
			ok: true,
			value: {
				subject: 'S',
				body: 'B',
				subjectB: 'SB',
				preheader: 'P',
				firstComment: 'FC',
				hashtags: ['#a', '#b'],
				media: { kind: 'carousel', imageUrls: ['https://x/1.png'] },
				scheduledIso: '2026-11-03T09:00:00+01:00',
			},
		});
	});

	it('takes scheduledIso null as "unscheduled"', () => {
		expect(paActionPatchFromArgs({ scheduledIso: null })).toEqual({
			ok: true,
			value: { scheduledIso: null },
		});
	});

	it('refuses an allowed key of the wrong type rather than dropping it', () => {
		for (const [patch, key] of [
			[{ subject: 1 }, 'subject'],
			[{ body: null }, 'body'],
			[{ hashtags: '#a' }, 'hashtags'],
			[{ hashtags: ['#a', 2] }, 'hashtags'],
			[{ media: { kind: 'reel', imageUrls: [] } }, 'media'],
			[{ media: { kind: 'single' } }, 'media'],
			[{ media: null }, 'media'],
			[{ scheduledIso: 'tomorrow' }, 'scheduledIso'],
			[{ scheduledIso: 5 }, 'scheduledIso'],
		] as const) {
			const r = paActionPatchFromArgs(patch);
			expect(r.ok, JSON.stringify(patch)).toBe(false);
			if (!r.ok) expect(r.error).toContain(`patch.${key}`);
		}
	});

	it('refuses a patch that is not an object or has nothing editable', () => {
		for (const raw of [undefined, null, 'x', [1], 3]) {
			expect(paActionPatchFromArgs(raw)).toEqual({ ok: false, error: '`patch` must be an object' });
		}
		expect(paActionPatchFromArgs({ junk: 1 })).toEqual({
			ok: false,
			error: '`patch` has no editable keys',
		});
	});
});

describe('paDraftSeenFromArgs (host.paActions.commit share binding)', () => {
	const SEEN = { channel: 'smtp', payloadJson: '{"item":{}}', editedJson: null, scheduledAt: null };

	it('passes the four hashed columns through verbatim and nothing else', () => {
		expect(paDraftSeenFromArgs({ ...SEEN, editedJson: '{"body":"x"}', id: 'extra' })).toEqual({
			ok: true,
			value: { ...SEEN, editedJson: '{"body":"x"}' },
		});
	});

	it('is optional', () => {
		expect(paDraftSeenFromArgs(undefined)).toEqual({ ok: true, value: undefined });
		expect(paDraftSeenFromArgs(null)).toEqual({ ok: true, value: undefined });
	});

	it('refuses a malformed row', () => {
		for (const raw of [
			'x',
			[],
			{ ...SEEN, channel: 1 },
			{ ...SEEN, payloadJson: {} },
			{ ...SEEN, editedJson: 3 },
		]) {
			expect(paDraftSeenFromArgs(raw).ok).toBe(false);
		}
	});
});

describe('rejectReasonFromArgs', () => {
	it('trims, treats blank as none, refuses a non-string', () => {
		expect(rejectReasonFromArgs('  Wrong tone ')).toEqual({ ok: true, value: 'Wrong tone' });
		expect(rejectReasonFromArgs('   ')).toEqual({ ok: true, value: undefined });
		expect(rejectReasonFromArgs(undefined)).toEqual({ ok: true, value: undefined });
		expect(rejectReasonFromArgs(7).ok).toBe(false);
	});
});
