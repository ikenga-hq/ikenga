// /ngwa/ — Ngwa index route (WP-10).
//
// Redirects /ngwa to /ngwa/installed, preserving search params and hash.

import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/ngwa/')({
	beforeLoad: ({ search, location }) => {
		throw redirect({
			to: '/ngwa/installed',
			search,
			hash: location.hash,
		});
	},
});
