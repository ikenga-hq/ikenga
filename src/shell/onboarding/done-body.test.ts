// done-body — the summary shows only completed steps' writes. Written under
// DEC-50; not run in the WP-38 round-2 fix pass.

import { describe, expect, it } from 'vitest';

import type { OnboardingStepId, OnboardingStepRecord } from '@/lib/shell/shell-store';

import { buildCards } from './done-body';
import type { ProjectStepPayload } from './project-body';
import { commitErrorMessage } from './wizard-stepper';

const CTX = { extraRoots: [], theme: 'A', mode: 'dark', density: 'default' };

function steps(
	overrides: Partial<Record<OnboardingStepId, OnboardingStepRecord>>
): Record<OnboardingStepId, OnboardingStepRecord> {
	const pending: OnboardingStepRecord = { status: 'pending' };
	return {
		welcome: pending,
		engine: pending,
		project: pending,
		equipment: pending,
		look: pending,
		shortcuts: pending,
		done: pending,
		...overrides,
	};
}

const DETECTED: ProjectStepPayload = {
	extraRoots: [],
	mode: 'detected',
	projectId: null,
	projectName: 'repo-a',
	projectRoot: '/src/repo-a',
	scope: 'project',
};

describe('buildCards', () => {
	it('does not name a project the user left without committing', () => {
		const cards = buildCards(
			steps({ project: { status: 'in_progress', payload: DETECTED } }),
			CTX
		);
		const project = cards.find((c) => c.id === 'project');
		expect(project?.value).toBe('Not answered');
		expect(project?.value).not.toContain('repo-a');
	});

	it('names the committed project', () => {
		const cards = buildCards(
			steps({ project: { status: 'completed', payload: DETECTED } }),
			CTX
		);
		expect(cards.find((c) => c.id === 'project')?.value).toContain('repo-a');
	});

	it('keeps Skipped for skipped steps', () => {
		const cards = buildCards(steps({ equipment: { status: 'skipped' } }), CTX);
		expect(cards.find((c) => c.id === 'equipment')?.value).toBe('Skipped');
	});
});

describe('commitErrorMessage', () => {
	it('surfaces Error messages and Tauri string rejections', () => {
		expect(commitErrorMessage(new Error('root_path already in use'))).toBe(
			'root_path already in use'
		);
		expect(commitErrorMessage('project id taken')).toBe('project id taken');
	});

	it('falls back to generic copy for anything else', () => {
		expect(commitErrorMessage(undefined)).toMatch(/try again/);
		expect(commitErrorMessage({})).toMatch(/try again/);
	});
});
