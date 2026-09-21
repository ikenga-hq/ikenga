// Settings → Packages → Health (DEC-16 / WP-16).
// Folded under /ngwa/health?section=sidecars.

import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/pkg-health')({
	beforeLoad: ({ location }) => {
		throw redirect({
			to: '/ngwa/health',
			search: { section: 'sidecars' },
			hash: location.hash,
		});
	},
});
