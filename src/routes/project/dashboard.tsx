// /project/dashboard — Project Obi dashboard (WP-10).
//
// Mounts the Obi canvas and home widgets from shell/home/home.tsx, with the
// D-04 daily address (WP-39) as the first widget row above it.
// Opened on project switch and via ⌘1 / `iyke go /project/dashboard`.

import { createFileRoute } from '@tanstack/react-router';
import { DailyAddress } from '@/shell/home/daily-address';
import { Home } from '@/shell/home/home';

function ProjectDashboardRoute() {
	return (
		<div className="flex h-full flex-col overflow-hidden">
			<div className="shrink-0 px-4 pt-4">
				<DailyAddress />
			</div>
			<div className="min-h-0 flex-1">
				<Home />
			</div>
		</div>
	);
}

export const Route = createFileRoute('/project/dashboard')({
	component: ProjectDashboardRoute,
});
