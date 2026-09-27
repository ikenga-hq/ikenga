import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/onboarding')({
	beforeLoad: () => {
		throw redirect({ to: '/settings/workspace' });
	},
});
