import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/pkg-audit')({
	beforeLoad: () => {
		throw redirect({
			to: '/ngwa/health',
			search: { section: 'violations' },
		});
	},
});
