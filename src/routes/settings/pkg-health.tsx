// Settings → Packages → Health (DEC-16 / WP-16 / WP-16a).
// Folded under /ngwa/health?section=violations: broken and orphaned install
// records render in the Violations panel (Install records), with Remove and
// Remove all behind a confirm.

import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/pkg-health')({
	beforeLoad: ({ location }) => {
		throw redirect({
			to: '/ngwa/health',
			search: { section: 'violations' },
			hash: location.hash,
		});
	},
});
