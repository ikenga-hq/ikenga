import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/terminal')({
	beforeLoad: () => {
		throw redirect({ to: '/settings/engines' });
	},
});
