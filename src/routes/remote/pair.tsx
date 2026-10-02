import { createFileRoute } from '@tanstack/react-router';

import { RemotePairPage } from './-components/remote-pair-page';

// G-ACCESS §3.12 (WP-74b): the device side of pairing. Public — a browser
// that opens `/remote/pair` cold is booted straight into the same page by
// `boot/primary.tsx`, before any credential exists.
export const Route = createFileRoute('/remote/pair')({
	component: () => <RemotePairPage />,
});
