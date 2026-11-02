// /outbox/approvals — the Outbox queue. Since the outbox redesign (plans/outbox-redesign
// D-01) the one triage inbox lives in the com.ikenga.outbound pkg; this shell route only
// deep-links into it (pkg route `/approvals`, view token `v:approvals`), like the other
// /outbox/* routes. It stays the stable target of the shell's entry points: the rail key,
// the status-bar count, ⌘K, the native menu and `pa-action-paused`.

import { createFileRoute } from '@tanstack/react-router';
import { CheckSquare } from 'lucide-react';
import { OutboundDeepLink } from '@/shell/outbox-deeplink';

export const Route = createFileRoute('/outbox/approvals')({
	component: () => (
		<OutboundDeepLink
			view="approvals"
			title="Outbox · Approvals"
			description="Drafts waiting for your decision, in the Outbound app."
			Icon={CheckSquare}
		/>
	),
});
