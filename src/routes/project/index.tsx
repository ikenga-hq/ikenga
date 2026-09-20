// /project/ — Project landing index route (WP-10).
//
// Redirects bare /project to /project/dashboard.

import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/project/')({
	beforeLoad: ({ search, location }) => {
		throw redirect({
			to: '/project/dashboard',
			search,
			hash: location.hash,
		});
	},
});
