// /project/dashboard — Project Obi dashboard (WP-10).
//
// Mounts the Obi canvas and home widgets from shell/home/home.tsx.
// Opened on project switch and via ⌘1 / `iyke go /project/dashboard`.

import { createFileRoute } from '@tanstack/react-router';
import { Home } from '@/shell/home/home';

function ProjectDashboardRoute() {
	return <Home />;
}

export const Route = createFileRoute('/project/dashboard')({
	component: ProjectDashboardRoute,
});
