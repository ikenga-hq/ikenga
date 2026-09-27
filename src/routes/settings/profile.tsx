import { createFileRoute } from '@tanstack/react-router';

import { ProfileTab } from '@/shell/people/profile';

// D-05 `profile` (WP-72): the local profile and App lock. The People section
// of the settings nav resolves here via `settingsSection`'s alias
// (`shell/settings/nav.tsx`).
export const Route = createFileRoute('/settings/profile')({
	component: ProfileTab,
});
