import { createFileRoute } from '@tanstack/react-router';

import { RemoteClient } from './-components/remote-client';

// G-ACCESS §3.12 / P-21 (WP-74b): a device grant below `full` boots here
// (`boot/primary.tsx`); a `full` device or a password session can open it by
// hand.
export const Route = createFileRoute('/remote/')({
	component: RemoteClient,
});
