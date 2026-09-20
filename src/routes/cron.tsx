// /cron — Legacy cron route (WP-10).
//
// Redirects /cron to /automations, preserving search params and hash.

import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/cron')({
	beforeLoad: ({ search, location }) => {
		throw redirect({
			to: '/automations',
			search,
			hash: location.hash,
		});
	},
});
