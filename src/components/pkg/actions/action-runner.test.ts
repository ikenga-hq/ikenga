import { afterEach, describe, expect, it, vi } from 'vitest';

const { runActionMock, handToChiMock } = vi.hoisted(() => ({ runActionMock: vi.fn(), handToChiMock: vi.fn() }));

vi.mock('@/lib/actions/runner', () => ({ runAction: runActionMock }));
vi.mock('@/shell/companion/companion-store', () => ({ handToChi: handToChiMock }));

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

describe('dispatchAction', () => {
	it('confirm and approve FILL the Companion dispatch bar and send nothing', async () => {
		for (const uxMode of ['confirm', 'approve'] as const) {
			await expect(dispatchAction(skillAction({ uxMode }))).resolves.toEqual({ ok: true, filled: true });
		}
		expect(handToChiMock).toHaveBeenCalledTimes(2);
		expect(handToChiMock).toHaveBeenCalledWith('/release-status report');
		expect(runActionMock).not.toHaveBeenCalled();
	});

	it('setup fills too (seed → review → send), interview flag included', async () => {
		await expect(
			dispatchAction(skillAction({ verb: 'setup', name: 'setup', uxMode: 'streaming' }), { interview: true })
		).resolves.toEqual({ ok: true, filled: true });
		expect(handToChiMock).toHaveBeenCalledWith('/release-status setup --interview');
		expect(runActionMock).not.toHaveBeenCalled();
	});

	it('only auto SENDS a chi dispatch invoking the skill, as scope "package" (ungated, never PTY), and returns the run id', async () => {
		runActionMock.mockResolvedValue({ status: 'done', kind: 'chi', testRun: false, runId: 'run-7' });
		await expect(dispatchAction(skillAction({ uxMode: 'auto' }))).resolves.toEqual({ ok: true, runId: 'run-7' });
		expect(handToChiMock).not.toHaveBeenCalled();
		expect(runActionMock).toHaveBeenCalledWith({
			id: 'com.x.release:release-status:report',
			name: 'Report',
			run: { kind: 'chi', target: 'active', prompt: '/release-status report' },
			// Package content: its own scope, so the runner never types it into a PTY.
			scope: 'package',
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
		expect(handToChiMock).not.toHaveBeenCalled();
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
