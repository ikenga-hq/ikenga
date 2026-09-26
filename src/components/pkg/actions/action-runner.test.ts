import { afterEach, describe, expect, it, vi } from 'vitest';

const { runActionMock } = vi.hoisted(() => ({ runActionMock: vi.fn() }));

vi.mock('@/lib/actions/runner', () => ({ runAction: runActionMock }));

import type { SkillAction } from '@/lib/tauri-cmd';
import { dispatchAction, skillActionPrompt, toDispatchResult } from './action-runner';

function skillAction(partial: Partial<SkillAction> = {}): SkillAction {
	return {
		pkgId: 'com.x.release',
		skill: 'release-status',
		verb: 'report',
		name: 'Report',
		uxMode: 'confirm',
		...partial,
	};
}

afterEach(() => {
	vi.clearAllMocks();
});

describe('dispatchAction delegates to the WP-53 runner', () => {
	it('sends a chi dispatch invoking the skill, ungated, and returns the run id', async () => {
		runActionMock.mockResolvedValue({ status: 'done', kind: 'chi', testRun: false, runId: 'run-7' });
		await expect(dispatchAction(skillAction())).resolves.toEqual({ ok: true, runId: 'run-7' });
		expect(runActionMock).toHaveBeenCalledWith({
			id: 'com.x.release:release-status:report',
			name: 'Report',
			run: { kind: 'chi', target: 'active', prompt: '/release-status report' },
			scope: 'personal',
		});
	});

	it('setup with interview adds the flag', () => {
		expect(skillActionPrompt(skillAction({ verb: 'setup', name: 'setup', uxMode: 'streaming' }), { interview: true })).toBe(
			'/release-status setup --interview'
		);
	});

	it('non-dispatchable modes never run', async () => {
		await expect(dispatchAction(skillAction({ uxMode: 'streaming' }))).resolves.toEqual({
			ok: false,
			reason: 'not-implemented',
		});
		expect(runActionMock).not.toHaveBeenCalled();
	});

	it('maps runner outcomes', () => {
		expect(toDispatchResult({ status: 'refused', kind: 'chi', reason: 'no-engine', message: 'No engine' })).toEqual({
			ok: false,
			reason: 'unavailable',
			message: 'No engine',
		});
		expect(toDispatchResult({ status: 'refused', kind: 'chi', reason: 'cancelled', message: '' })).toEqual({
			ok: false,
			reason: 'cancelled',
		});
		expect(toDispatchResult({ status: 'failed', kind: 'chi', testRun: false, message: 'boom' })).toEqual({
			ok: false,
			reason: 'failed',
			message: 'boom',
		});
	});
});
