import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/agent')({
	beforeLoad: () => {
		throw redirect({ to: '/settings/engines' });
	},
});
