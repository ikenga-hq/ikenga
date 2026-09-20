// summary-body — card builder + finish gating.

import { beforeEach, describe, expect, it } from 'vitest';

import { createDefaultOnboardingState, useShellStore } from '@/lib/shell/shell-store';

import type { AgentStepPayload } from './agent-body';
import type { AppearancePayload } from './appearance-body';
import type { ScaffoldingPayload } from './scaffolding-body';
import { buildCards, findBlockingState } from './summary-body';

const CTX = {
	extraRoots: ['~/royalti-co', '~/.company'],
	theme: 'A',
	mode: 'dark',
	density: 'comfortable',
};

beforeEach(() => {
	useShellStore.setState({ onboarding: createDefaultOnboardingState() });
});

describe('buildCards', () => {
	it('emits one card per step except summary', () => {
		const steps = useShellStore.getState().onboarding.steps;
		const cards = buildCards(steps, CTX);
		// 8 steps total minus the summary card.
		expect(cards).toHaveLength(7);
		expect(cards.map((c) => c.id)).not.toContain('summary');
	});

	it('renders the agent payload (display + path)', () => {
		const s = useShellStore.getState();
		const p: AgentStepPayload = {
			agentId: 'claude-code',
			display: 'Claude Code',
			executablePath: '/usr/local/bin/claude',
			version: '1.0.84',
			authed: true,
		};
		s.setOnboardingPayload('agent', p);
		s.markOnboardingStepCompleted('agent');

		const cards = buildCards(useShellStore.getState().onboarding.steps, CTX);
		const agentCard = cards.find((c) => c.id === 'agent');
		expect(agentCard?.value).toBe('Claude Code');
		expect(agentCard?.detail).toContain('/usr/local/bin/claude');
		expect(agentCard?.detail).toContain('v1.0.84');
	});

	it('marks skipped steps with `skipped: true`', () => {
		const s = useShellStore.getState();
		s.markOnboardingStepSkipped('connectors');
		const cards = buildCards(useShellStore.getState().onboarding.steps, CTX);
		const connectors = cards.find((c) => c.id === 'connectors');
		expect(connectors?.skipped).toBe(true);
		expect(connectors?.value).toBe('Skipped');
	});

	it('renders the appearance choice via the theme store snapshot', () => {
		const s = useShellStore.getState();
		const p: AppearancePayload = { theme: 'B', mode: 'light', density: 'compact' };
		s.setOnboardingPayload('appearance', p);
		s.markOnboardingStepCompleted('appearance');

		const cards = buildCards(useShellStore.getState().onboarding.steps, CTX);
		const ap = cards.find((c) => c.id === 'appearance');
		expect(ap?.value).toMatch(/Kola Daylight/);
		expect(ap?.value).toMatch(/Light/);
		expect(ap?.value).toMatch(/Compact/);
	});

	it('renders scaffolding payload choices', () => {
		const s = useShellStore.getState();
		const p: ScaffoldingPayload = {
			choice: 'merge',
			rootPath: '~/royalti-co/ikenga',
			profile: 'starter',
			at: 1_700_000_000_000,
		};
		s.setOnboardingPayload('scaffolding', p);
		s.markOnboardingStepCompleted('scaffolding');

		const cards = buildCards(useShellStore.getState().onboarding.steps, CTX);
		const sc = cards.find((c) => c.id === 'scaffolding');
		expect(sc?.value).toMatch(/Merged/);
		expect(sc?.detail).toBe('~/royalti-co/ikenga');
	});

	it('marks packages + connectors as pending Phase 5', () => {
		const cards = buildCards(useShellStore.getState().onboarding.steps, CTX);
		const pkg = cards.find((c) => c.id === 'packages');
		const conn = cards.find((c) => c.id === 'connectors');
		expect(pkg?.value).toMatch(/Phase 5/i);
		expect(conn?.value).toMatch(/Phase 5/i);
	});
});

describe('findBlockingState', () => {
	it('blocks when welcome is still pending', () => {
		const steps = useShellStore.getState().onboarding.steps;
		expect(findBlockingState(steps)).toMatch(/Consecration/i);
	});

	it('blocks when agent is still pending after welcome', () => {
		const s = useShellStore.getState();
		s.markOnboardingStepCompleted('welcome');
		const steps = useShellStore.getState().onboarding.steps;
		expect(findBlockingState(steps)).toMatch(/Chi/);
	});

	it('does not block once welcome is complete and agent is selected', () => {
		const s = useShellStore.getState();
		s.markOnboardingStepCompleted('welcome');
		s.markOnboardingStepCompleted('agent');
		const steps = useShellStore.getState().onboarding.steps;
		expect(findBlockingState(steps)).toBeNull();
	});
});

describe('finish — onboarding state transitions', () => {
	it('finishOnboarding stamps completedAt so the boot redirect stops firing', () => {
		expect(useShellStore.getState().onboarding.completedAt).toBeNull();
		useShellStore.getState().finishOnboarding();
		const after = useShellStore.getState().onboarding;
		expect(after.completedAt).not.toBeNull();
	});
});
