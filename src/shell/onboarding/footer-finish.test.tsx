// Audit 2026-10-06 rank 19: on the last onboarding step the footer's
// "Enter your Obi" only stamped completion and never left the wizard, while
// the in-page "Enter your Obi (open workspace)" button greeted the user and
// navigated to `/`. Both now run DoneBody's handleOpenWorkspace.

import { act, cleanup, fireEvent, render, screen } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const navigate = vi.fn();

vi.mock('@tanstack/react-router', () => ({
	useNavigate: () => navigate,
	Link: ({ children }: { children?: ReactNode }) => <a href="#x">{children}</a>,
}));

import { useShellStore } from '@/lib/shell/shell-store';

import { DoneBody } from './done-body';
import { WizardStepper } from './wizard-stepper';

function setStepStatus(status: 'completed' | 'pending') {
	useShellStore.setState((s) => ({
		onboarding: {
			...s.onboarding,
			steps: {
				...s.onboarding.steps,
				welcome: { ...s.onboarding.steps.welcome, status },
				engine: { ...s.onboarding.steps.engine, status },
			},
		},
	}));
}

beforeEach(() => {
	vi.useFakeTimers();
	navigate.mockReset();
	setStepStatus('completed');
});

afterEach(() => {
	cleanup();
	vi.useRealTimers();
	vi.restoreAllMocks();
});

describe('onboarding footer on the done step', () => {
	it('runs the handler the body registered, not a bare finish', () => {
		const finishOnboarding = vi.spyOn(useShellStore.getState(), 'finishOnboarding');
		const finish = vi.fn();
		function Body({ setFinish }: { setFinish: (fn: (() => void) | null) => void }) {
			setFinish(finish);
			return null;
		}
		render(
			<WizardStepper stepId="done">
				{({ setFinish }) => <Body setFinish={setFinish} />}
			</WizardStepper>
		);
		const next = screen.getByTestId('wizard-next');
		expect(next.textContent).toBe('Enter your Obi');
		fireEvent.click(next);
		expect(finish).toHaveBeenCalledTimes(1);
		expect(finishOnboarding).not.toHaveBeenCalled();
	});

	it('enters the workspace from the footer exactly like the in-page button', () => {
		const onFinish = vi.fn();
		render(
			<WizardStepper stepId="done">
				{({ goTo, setFinish }) => (
					<DoneBody onFinish={onFinish} goTo={goTo} setFinish={setFinish} />
				)}
			</WizardStepper>
		);

		fireEvent.click(screen.getByTestId('wizard-next'));
		// The greeting flourish shows first, then the route changes.
		expect(screen.getByTestId('summary-greeting-flourish')).toBeTruthy();
		expect(navigate).not.toHaveBeenCalled();
		act(() => {
			vi.advanceTimersByTime(700);
		});
		expect(onFinish).toHaveBeenCalledTimes(1);
		expect(navigate).toHaveBeenCalledWith({ to: '/' });

		// A second click during or after the flourish does not finish twice.
		fireEvent.click(screen.getByTestId('wizard-next'));
		act(() => {
			vi.advanceTimersByTime(700);
		});
		expect(onFinish).toHaveBeenCalledTimes(1);
	});

	it('is held by the same blocker as the in-page button', () => {
		setStepStatus('pending');
		const onFinish = vi.fn();
		render(
			<WizardStepper stepId="done">
				{({ goTo, setFinish }) => (
					<DoneBody onFinish={onFinish} goTo={goTo} setFinish={setFinish} />
				)}
			</WizardStepper>
		);
		expect(screen.getByTestId('summary-blocker')).toBeTruthy();
		fireEvent.click(screen.getByTestId('wizard-next'));
		act(() => {
			vi.advanceTimersByTime(700);
		});
		expect(onFinish).not.toHaveBeenCalled();
		expect(navigate).not.toHaveBeenCalled();
	});
});
