// Settings → Data health (DEC-16 / WP-16).
// Folded under /ngwa/health?section=data.

import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/data-health')({
	beforeLoad: ({ location }) => {
		throw redirect({
			to: '/ngwa/health',
			search: { section: 'data' },
			hash: location.hash,
		});
	},
});
