// done-body — the summary shows only completed steps' writes. Written under
// DEC-50; not run in the WP-38 round-2 fix pass.

import { describe, expect, it } from 'vitest';

import type { OnboardingStepId, OnboardingStepRecord } from '@/lib/shell/shell-store';

import { buildCards } from './done-body';
import type { EquipmentStepPayload } from './equipment-body';
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

// Gap audit rank 3 — the Done step used to count the packages the user PICKED
// ("3 packages"), so an install batch that failed (or never ran) read as done.
describe('Ngwa card install honesty (gap rank 3)', () => {
	const sel = ['com.ikenga.tasks', 'com.ikenga.mail'];
	const equipment = (installResults?: EquipmentStepPayload['installResults']) =>
		steps({
			equipment: {
				status: 'completed',
				payload: {
					selected: sel,
					connectorsConfigured: [],
					connectorsSkipped: [],
					installResults,
				} satisfies EquipmentStepPayload,
			},
		});
	const card = (s: ReturnType<typeof steps>) =>
		buildCards(s, CTX).find((c) => c.id === 'equipment');

	it('reports only recorded successes and lists each failure with its real reason', () => {
		const c = card(
			equipment([
				{ pkgId: sel[0], display: 'Tasks', ok: true, skipped: false },
				{
					pkgId: sel[1],
					display: 'Mail',
					ok: false,
					skipped: false,
					error: 'Not available on this server yet',
				},
			])
		);
		expect(c?.value).toBe('1 of 2 packages installed');
		expect(c?.problems).toEqual(['Mail: Not available on this server yet']);
	});

	it('never says installed when no result was recorded', () => {
		const c = card(equipment(undefined));
		expect(c?.value).toBe('2 packages selected');
		expect(c?.value).not.toMatch(/installed/);
		expect(c?.problems?.[0]).toMatch(/status unknown/i);
	});

	it('has no problems when everything installed', () => {
		const c = card(
			equipment(sel.map((pkgId) => ({ pkgId, display: pkgId, ok: true, skipped: false })))
		);
		expect(c?.value).toBe('2 of 2 packages installed');
		expect(c?.problems).toEqual([]);
	});
});
