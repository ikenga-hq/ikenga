// Sign-in state chip on an onboarding engine card. Its own module so it can
// be tested without engine-body's heavy imports.

import { StatusChip } from '@/components/ui/status-chip';

export function AuthPill({ authed, hint }: { authed: boolean | null; hint: string | null }) {
	if (authed === true) {
		return <StatusChip tone="live">signed in</StatusChip>;
	}
	if (authed === false) {
		return <StatusChip tone="warn">auth required</StatusChip>;
	}
	// `null` with a hint is an inconclusive probe — say "couldn't tell" with
	// the probe's reason. `null` without one is an engine that has no sign-in
	// probe at all: nothing to report, so render nothing.
	if (!hint) return null;
	return (
		<span title={hint} data-testid="auth-pill-unknown">
			<StatusChip tone="faint">sign-in unknown</StatusChip>
		</span>
	);
}
