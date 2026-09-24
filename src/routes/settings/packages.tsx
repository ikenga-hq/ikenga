import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/packages')({
	beforeLoad: () => {
		throw redirect({ to: '/ngwa/store' });
	},
});
