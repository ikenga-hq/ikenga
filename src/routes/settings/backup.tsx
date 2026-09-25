import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/backup')({
	beforeLoad: () => {
		throw redirect({ to: '/settings/storage' });
	},
});
