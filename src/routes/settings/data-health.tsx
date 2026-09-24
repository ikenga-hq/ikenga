import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/data-health')({
	beforeLoad: () => {
		throw redirect({
			to: '/ngwa/health',
			search: { section: 'data' },
		});
	},
});
