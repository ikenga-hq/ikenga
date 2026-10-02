import { createFileRoute } from '@tanstack/react-router';

import { DevicesTab } from '@/shell/people/devices';

// D-05 `devices`: WP-72's read-only Remote access over what the daemon
// exposes, plus pairing and the per-device grants table (WP-74b, G-ACCESS §3).
export const Route = createFileRoute('/settings/devices')({
	component: DevicesTab,
});
