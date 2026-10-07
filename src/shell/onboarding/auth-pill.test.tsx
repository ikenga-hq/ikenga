// AuthPill: `authed === null` covers two cases — an inconclusive probe (has a
// hint: say "couldn't tell") and an engine with no sign-in probe at all (no
// hint: nothing to report, so no pill).

import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { AuthPill } from './auth-pill';

afterEach(cleanup);

describe('AuthPill', () => {
	it('shows "sign-in unknown" with the reason when the probe was inconclusive', () => {
		render(<AuthPill authed={null} hint="auth probe `claude` timed out after 5000ms" />);
		const pill = screen.getByTestId('auth-pill-unknown');
		expect(pill.textContent).toMatch(/sign-in unknown/);
		expect(pill.getAttribute('title')).toMatch(/timed out/);
	});

	it('renders nothing for an engine with no sign-in probe', () => {
		const { container } = render(<AuthPill authed={null} hint={null} />);
		expect(container.textContent).toBe('');
	});

	it('keeps the confident states', () => {
		render(<AuthPill authed={true} hint={null} />);
		expect(screen.getByText('signed in')).toBeTruthy();
		cleanup();
		render(<AuthPill authed={false} hint="missing: ~/.claude/.credentials.json" />);
		expect(screen.getByText('auth required')).toBeTruthy();
	});
});
