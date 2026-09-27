import { createFileRoute, redirect } from '@tanstack/react-router';

// D-05 People, devices and access. WP-72 builds its local tabs as their own
// routes, `/settings/profile` and `/settings/devices`. The section lands on
// Devices, the design's default tab (`designs/people.html`). This route used
// to render a summary card of static placeholders; it now only redirects.
// Members, Policies and Audit arrive with G-ACCESS (WP-76, WP-77).
export const Route = createFileRoute('/settings/people')({
	beforeLoad: () => {
		throw redirect({ to: '/settings/devices' });
	},
});
