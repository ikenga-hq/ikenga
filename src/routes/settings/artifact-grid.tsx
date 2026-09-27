import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/artifact-grid')({
	beforeLoad: () => {
		throw redirect({ to: '/settings/workspace' });
	},
});
