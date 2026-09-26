// /settings/actions — redirects to the default tab, same pattern as
// `/ngwa/index.tsx` redirecting to `/ngwa/installed` (WP-57).

import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/actions')({
	beforeLoad: ({ search, location }) => {
		throw redirect({
			to: '/settings/actions/$tab',
			params: { tab: 'actions' },
			search,
			hash: location.hash,
		});
	},
});
