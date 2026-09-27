// WP-55 — a menu-run action's outcome is never dropped: a refusal or a
// failure becomes a notice, and a trust refusal also opens the trust sheet.

import { beforeEach, describe, expect, it } from 'vitest';
import type { RunOutcome } from '@/lib/actions/runner';
import { surfaceRunOutcome, useMenuRunNotice } from './run-notice';

const trustSheet = { mode: 'project-actions' as const, projectId: 'p1', actionIds: ['deploy'] };

beforeEach(() => {
	useMenuRunNotice.setState({ notice: null, trustSheet: null });
});

describe('surfaceRunOutcome', () => {
	it('shows a trust refusal and opens the trust sheet on the refused action', () => {
		surfaceRunOutcome({
			status: 'refused',
			kind: 'shell',
			reason: 'untrusted',
			message: 'This project’s actions are not trusted yet.',
			trustSheet,
		});
		const s = useMenuRunNotice.getState();
		expect(s.notice?.message).toBe('This project’s actions are not trusted yet.');
		expect(s.trustSheet).toEqual(trustSheet);
	});

	it('shows a non-trust refusal (no-target) without opening the sheet', () => {
		surfaceRunOutcome({ status: 'refused', kind: 'chi', reason: 'no-target', message: 'No Chi to send to.' });
		const s = useMenuRunNotice.getState();
		expect(s.notice?.message).toBe('No Chi to send to.');
		expect(s.trustSheet).toBeNull();
	});

	it('shows a failure', () => {
		surfaceRunOutcome({ status: 'failed', kind: 'shell', testRun: false, message: 'exit 2' });
		expect(useMenuRunNotice.getState().notice?.message).toBe('exit 2');
	});

	it('stays silent for done and for the user’s own cancel', () => {
		surfaceRunOutcome({ status: 'done', kind: 'open', testRun: false } as RunOutcome);
		surfaceRunOutcome({ status: 'refused', kind: 'shell', reason: 'cancelled', message: 'Cancelled.' });
		expect(useMenuRunNotice.getState().notice).toBeNull();
	});

	it('a repeat of the same message is a new notice (restarts its timer)', () => {
		surfaceRunOutcome({ status: 'failed', kind: 'shell', testRun: false, message: 'x' });
		const first = useMenuRunNotice.getState().notice?.seq;
		surfaceRunOutcome({ status: 'failed', kind: 'shell', testRun: false, message: 'x' });
		expect(useMenuRunNotice.getState().notice?.seq).not.toBe(first);
	});
});
