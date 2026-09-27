import { createFileRoute } from '@tanstack/react-router';

import { DevicesTab } from '@/shell/people/devices';

// D-05 `devices` (WP-72): read-only Remote access over what the daemon
// exposes, labelled as not a security boundary. There is no pairing yet
// (WP-74, behind G-ACCESS).
export const Route = createFileRoute('/settings/devices')({
	component: DevicesTab,
});
