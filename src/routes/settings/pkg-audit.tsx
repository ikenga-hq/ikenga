// Settings → Packages → Violations audit (DEC-16 / WP-16).
// Folded under /ngwa/health?section=violations.

import { createFileRoute, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/pkg-audit')({
	beforeLoad: ({ location }) => {
		throw redirect({
			to: '/ngwa/health',
			search: { section: 'violations' },
			hash: location.hash,
		});
	},
});
