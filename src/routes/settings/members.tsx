import { createFileRoute } from '@tanstack/react-router';

import { MembersTab } from '@/shell/people/members';

// D-05 `members` / `members-shared` / `share-kola` (G-ACCESS §4, §7, WP-76):
// the project's people, roles and invites, plus "Shared with you".
export const Route = createFileRoute('/settings/members')({
	component: MembersTab,
});
