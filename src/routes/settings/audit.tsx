import { createFileRoute } from '@tanstack/react-router';

import { AuditTab } from '@/shell/people/audit';

// D-05 `audit` (G-ACCESS §6, WP-77): the append-only access audit log —
// filters by who, device and kind, search, export, and the degraded banner.
export const Route = createFileRoute('/settings/audit')({
	component: AuditTab,
});
