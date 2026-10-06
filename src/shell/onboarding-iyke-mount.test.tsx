// Gap audit rank 25 (onboarding half) — `OnboardingIykeMount` pushed
// `iyke_set_shell` on every onboarding route change with no desktop gate, so a
// browser session against the headless daemon fired a failing RPC
// ("not implemented in headless daemon") at each wizard step. The push is now
// desktop-only; the workspace half is covered by
// workspace-effects.iyke-sync.test.ts.

import { cleanup, render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	setShell: vi.fn(async (_args: unknown) => {}),
}));

vi.mock('@/lib/iyke/client', async (orig) => ({
	...(await orig<typeof import('@/lib/iyke/client')>()),
	setShell: h.setShell,
}));
vi.mock('@/lib/iyke/bridge', () => ({ useIykeBridge: () => {} }));

import { OnboardingIykeMount } from './onboarding-iyke-mount';

beforeEach(() => h.setShell.mockClear());
afterEach(() => cleanup());

describe('OnboardingIykeMount iyke_set_shell (gap rank 25)', () => {
	it('sends nothing outside the desktop app, across route changes', () => {
		const { rerender } = render(<OnboardingIykeMount path="/onboarding" desktop={false} />);
		rerender(<OnboardingIykeMount path="/onboarding/agent" desktop={false} />);
		rerender(<OnboardingIykeMount path="/onboarding/done" desktop={false} />);
		expect(h.setShell).not.toHaveBeenCalled();
	});

	it('defaults to the browser side when isTauri() is false', () => {
		render(<OnboardingIykeMount path="/onboarding" />);
		expect(h.setShell).not.toHaveBeenCalled();
	});

	it('still publishes the literal onboarding route on the desktop', () => {
		const { rerender } = render(<OnboardingIykeMount path="/onboarding" desktop />);
		rerender(<OnboardingIykeMount path="/onboarding/agent" desktop />);
		expect(h.setShell).toHaveBeenCalledTimes(2);
		expect(h.setShell).toHaveBeenLastCalledWith({
			mode: 'onboarding',
			route: '/onboarding/agent',
			panes: null,
			sidebarCollapsed: true,
		});
	});
});
