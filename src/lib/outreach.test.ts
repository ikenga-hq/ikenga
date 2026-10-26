import { describe, expect, it } from 'vitest';
import {
	type MaybeOutreach,
	orderOutreach,
	outreachHoldNote,
	outreachOf,
	outreachPauseText,
	outreachStateLabel,
} from './outreach';

const block = (assignee: string, send_rank?: number) => ({
	outreach: { assignee, ...(send_rank === undefined ? {} : { send_rank }) },
});

describe('outreachOf', () => {
	it('reads the hash-bound block and keeps only well-typed members', () => {
		expect(
			outreachOf({
				outreach: {
					assignee: 'ada',
					from_identity: 'adaugo',
					send_rank: 4,
					template: 'C',
					region: 'uk-eu',
					company: 'Pub',
					step: 'first',
					extra: 'x',
				},
			})
		).toEqual({
			assignee: 'ada',
			from_identity: 'adaugo',
			send_rank: 4,
			template: 'C',
			region: 'uk-eu',
			company: 'Pub',
			step: 'first',
		});
		expect(outreachOf({ outreach: { assignee: 'ada', send_rank: '4', company: 7 } })).toEqual({
			assignee: 'ada',
		});
	});

	it('is undefined without a usable assignee', () => {
		for (const item of [
			null,
			undefined,
			'x',
			{},
			{ outreach: null },
			{ outreach: 'ada' },
			{ outreach: {} },
			{ outreach: { assignee: '' } },
			{ outreach: { assignee: 3 } },
		]) {
			expect(outreachOf(item)).toBeUndefined();
		}
	});
});

describe('orderOutreach', () => {
	const ids = (rows: { id: string }[]) => rows.map((r) => r.id).join(',');

	it('orders one queue by send_rank ascending', () => {
		const rows = [
			{ id: 'r3', ...block('ada', 3) },
			{ id: 'r1', ...block('ada', 1) },
			{ id: 'r10', ...block('ada', 10) },
			{ id: 'r2', ...block('ada', 2) },
		];
		expect(ids(orderOutreach(rows))).toBe('r1,r2,r3,r10');
	});

	it('groups an all-view by assignee, then rank', () => {
		const rows = [
			{ id: 'c2', ...block('chinedum', 2) },
			{ id: 'a2', ...block('ada', 2) },
			{ id: 'c1', ...block('chinedum', 1) },
			{ id: 'a1', ...block('ada', 1) },
		];
		expect(ids(orderOutreach(rows))).toBe('a1,a2,c1,c2');
	});

	it('puts an unranked row last in its queue and keeps ties in the order given', () => {
		const rows = [
			{ id: 'none', ...block('ada') },
			{ id: 'tie-b', ...block('ada', 2) },
			{ id: 'tie-a', ...block('ada', 2) },
			{ id: 'one', ...block('ada', 1) },
		];
		expect(ids(orderOutreach(rows))).toBe('one,tie-b,tie-a,none');
	});

	it('leaves rows that are not outreach exactly where they were', () => {
		const rows: (MaybeOutreach & { id: string })[] = [
			{ id: 'plain-1' },
			{ id: 'o3', ...block('ada', 3) },
			{ id: 'plain-2' },
			{ id: 'o1', ...block('ada', 1) },
			{ id: 'plain-3' },
		];
		expect(ids(orderOutreach(rows))).toBe('plain-1,o1,plain-2,o3,plain-3');
	});

	it('does not change its input', () => {
		const rows = [
			{ id: 'b', ...block('ada', 2) },
			{ id: 'a', ...block('ada', 1) },
		];
		orderOutreach(rows);
		expect(ids(rows)).toBe('b,a');
	});
});

describe('labels', () => {
	it('names the sender states and says nothing for the rest', () => {
		expect(outreachStateLabel({ state: 'queued_for_tomorrow' })).toBe('queued for tomorrow');
		expect(outreachStateLabel({ state: 'suppressed', reason: 'suppressed:domain' })).toBe(
			'suppressed'
		);
		expect(outreachStateLabel({ state: 'paused' })).toBe('paused');
		expect(outreachStateLabel({ state: 'sent' })).toBeNull();
		expect(outreachStateLabel({ state: 'something-new' })).toBeNull();
		expect(outreachStateLabel(undefined)).toBeNull();
	});

	it('never claims a held row is sending', () => {
		for (const state of ['queued_for_tomorrow', 'paused', 'suppressed', 'blocked']) {
			expect(outreachHoldNote({ state })).not.toMatch(/sending$/);
			expect(outreachHoldNote({ state })).not.toBeNull();
		}
		expect(outreachHoldNote({ state: 'sent' })).toBeNull();
		expect(outreachHoldNote(undefined)).toBeNull();
	});

	it('words the pause banner, with progress when the sender gave it', () => {
		expect(outreachPauseText(undefined)).toBeNull();
		expect(outreachPauseText({})).toBe('Sending is paused until Chinedum resumes.');
		expect(outreachPauseText({ sentCount: 30, threshold: 30 })).toBe(
			'Sending is paused until Chinedum resumes (30 of 30 sent).'
		);
	});
});
