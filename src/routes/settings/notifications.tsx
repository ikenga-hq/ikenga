import { createFileRoute } from '@tanstack/react-router';

import { NotificationsSettings } from '@/shell/pwa/notifications-settings';

// plans/pwa S4 §3: push notifications to this device, per-event toggles, a
// test, and the devices that get them.
export const Route = createFileRoute('/settings/notifications')({
	component: NotificationsSettings,
});
