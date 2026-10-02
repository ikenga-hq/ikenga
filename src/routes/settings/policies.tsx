import { createFileRoute } from '@tanstack/react-router';

import { PoliciesTab } from '@/shell/people/policies';

// D-05 `policies` (G-ACCESS §4.1, §5.2, WP-76): what each role may do in this
// project, and the "Require Owner approval" limit.
export const Route = createFileRoute('/settings/policies')({
	component: PoliciesTab,
});
