import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/activity-bar')({
	beforeLoad: () => {
		throw redirect({ to: '/settings/workspace' });
	},
});
