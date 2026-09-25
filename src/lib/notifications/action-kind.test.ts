// WP-40 — action narrowing. Written under DEC-50; not run until the 5b close
// (WP-47).

import { describe, expect, it } from 'vitest';
import type { KnownNotificationAction, NotificationAction } from '@/lib/tauri-cmd';
import { asKnownNotificationAction, KNOWN_NOTIFICATION_ACTION_KINDS } from './action-kind';

describe('asKnownNotificationAction', () => {
	it('returns null for no action or an unknown kind', () => {
		expect(asKnownNotificationAction(null)).toBeNull();
		expect(asKnownNotificationAction(undefined)).toBeNull();
		expect(asKnownNotificationAction({ kind: 'some.future.action', foo: 'bar' })).toBeNull();
	});

	it('passes every known kind through unchanged', () => {
		const chi: NotificationAction = {
			kind: 'open.chi_run',
			runId: 'r1',
			status: 'done',
			artifactCount: 2,
			firstArtifactPath: '/tmp/a.png',
		};
		expect(asKnownNotificationAction(chi)).toBe(chi);
		expect(KNOWN_NOTIFICATION_ACTION_KINDS).toContain('permission.decide');
	});

	it('narrows on kind and via so params are typed', () => {
		const raw: NotificationAction = {
			kind: 'permission.decide',
			via: 'acp',
			threadId: 'th-1',
			requestId: 'req-1',
			terminalId: null,
		};
		const a = asKnownNotificationAction(raw);
		// Compile-time: after narrowing, `threadId` is `string`, not `unknown`.
		let threadId: string | null = null;
		if (a?.kind === 'permission.decide' && a.via === 'acp') threadId = a.threadId;
		expect(threadId).toBe('th-1');
	});

	it('reads a permission.decide without a recognised via as hooks', () => {
		const a = asKnownNotificationAction({
			kind: 'permission.decide',
			requestId: 'req-2',
			terminalId: null,
		} as NotificationAction);
		expect(a).toEqual<KnownNotificationAction>({
			kind: 'permission.decide',
			via: 'hooks',
			requestId: 'req-2',
			terminalId: null,
		});
	});
});
