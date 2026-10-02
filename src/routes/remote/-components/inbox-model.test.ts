import { describe, expect, it } from 'vitest';
import type { Cap, Tier } from '@/lib/access/caps.gen';
import type { AccessStatus } from '@/lib/access/client';

import {
	decideErrorCopy,
	type InboxRow,
	inboxCardReason,
	needsRoutedDevice,
	offersAlways,
} from './inbox-model';

function status(tier: Tier, caps: Cap[]): AccessStatus {
	return {
		tier: 't0',
		store: 'ok',
		principal: { principalId: 'p', username: 'ned', isAdmin: false },
		credential: { via: 'device', deviceId: 'pixel', tier },
		caps,
		adminStrength: false,
		publicUrl: null,
		sharingEnabled: false,
		share: null,
	};
}

function row(over: Partial<InboxRow> = {}): InboxRow {
	return {
		id: 1,
		kind: 'permission',
		title: 'Claude wants to use Edit',
		body: null,
		action: null,
		source: 'relay',
		dedupeKey: 'permission:relay:permission:acp:t:r',
		count: 1,
		createdAt: 0,
		updatedAt: 0,
		readAt: null,
		resolvedAt: null,
		...over,
	};
}

const approve = status('approve', ['files', 'sessions', 'dispatch', 'approve']);
const dispatch = status('dispatch', ['files', 'sessions', 'dispatch']);

describe('remote inbox read model (G-ACCESS §5.7)', () => {
	it('a live card has no reason', () => {
		expect(inboxCardReason(row({ can_decide: true }), approve, null)).toBeNull();
	});

	it('names what the card waits on', () => {
		expect(inboxCardReason(row({ can_decide: false, waiting_on: 'owner' }), approve, null)).toBe(
			'Waiting on the Owner'
		);
		expect(
			inboxCardReason(row({ can_decide: false, waiting_on: 'device' }), approve, 'ned-desktop')
		).toBe('Answer on ned-desktop (this device only)');
		expect(
			inboxCardReason(row({ can_decide: false, waiting_on: 'device' }), approve, null)
		).toMatch(/this device only/);
		expect(inboxCardReason(row({ can_decide: false, waiting_on: 'approve' }), dispatch, null)).toBe(
			"This device can't approve — it is View + dispatch"
		);
		expect(
			inboxCardReason(
				row({ can_decide: false, waiting_on: null, dedupeKey: 'permission:terminal:x' }),
				approve,
				null
			)
		).toMatch(/terminal/);
		expect(inboxCardReason(row({ resolvedAt: 3, can_decide: true }), approve, null)).toBe(
			'Already answered'
		);
	});

	it('offers "always for this project" only when the row says so', () => {
		expect(offersAlways(row({ can_decide: true, can_allow_always: true }))).toBe(true);
		expect(offersAlways(row({ can_decide: true, can_allow_always: false }))).toBe(false);
		expect(offersAlways(row({ can_decide: false, can_allow_always: true }))).toBe(false);
		expect(offersAlways(row({ can_decide: true }))).toBe(false);
	});

	it('looks the routed device up only when a card waits on it', () => {
		expect(needsRoutedDevice([row({ waiting_on: 'device' })])).toBe(true);
		expect(
			needsRoutedDevice([
				row({ waiting_on: 'device', resolvedAt: 1 }),
				row({ waiting_on: 'owner' }),
			])
		).toBe(false);
	});

	it('words the decide refusals', () => {
		expect(decideErrorCopy('conflict', 'x')).toMatch(/timed out/);
		expect(decideErrorCopy('owner_approval_required', 'x')).toMatch(/Owner/);
		expect(decideErrorCopy(null, 'boom')).toBe('boom');
	});
});
