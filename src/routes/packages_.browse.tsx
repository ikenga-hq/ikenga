// Legacy /packages/browse → /packages (the unified surface).
// The mission-control catalog already lists registry pkgs alongside
// installed ones; this redirect keeps old links + the nav-config entry
// pointing somewhere alive while we drop them.

import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/packages_/browse')({
	beforeLoad: ({ search, location }) => {
		throw redirect({
			to: '/ngwa/store',
			search,
			hash: location.hash,
		});
	},
});
