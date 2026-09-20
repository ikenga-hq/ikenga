// /agent-runs — Legacy agent-runs route (WP-10).
//
// Redirects /agent-runs to /automations (view=runs), preserving search params and hash.

import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/agent-runs')({
	beforeLoad: ({ search, location }) => {
		throw redirect({
			to: '/automations',
			search: {
				...search,
				view: 'runs',
			},
			hash: location.hash,
		});
	},
});
