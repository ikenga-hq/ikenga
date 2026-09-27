import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/pkg-health')({
	beforeLoad: () => {
		throw redirect({
			to: '/ngwa/health',
			search: { section: 'violations' },
		});
	},
});
