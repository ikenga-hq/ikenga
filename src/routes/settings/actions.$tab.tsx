// /settings/actions/$tab — D-06 Actions, menus and keys (WP-57).
//
// Four tabs: actions (default) · editor · menus · keys. An unknown `$tab`
// redirects to `actions` rather than 404ing, same spirit as `/settings`'s
// own index redirect. `editor`, `menus`, `keys` and `import` bodies are
// WP-58..61's (`src/shell/actions/{editor,menus,keys,import}/`, do-not-touch
// here) — this route only wires the shell and the `actions` / `empty`
// states (G-ACTIONS §11, D-06 states `actions` and `empty`).
//
// NOTE: this route does not typecheck against the stale `routeTree.gen.ts`
// until the orchestrator regenerates it (WP-57 brief, "Route file" note) —
// expected, not a defect in this PR.

import { createFileRoute, redirect } from '@tanstack/react-router';
import { z } from 'zod';
import { ActionsShell } from '@/shell/actions/shell';
import { isActionsTab, type ActionsTabId } from '@/shell/actions/types';

const actionsTabSearchSchema = z.object({
	/** An action id to focus once the Editor tab (WP-58) reads it — plumbed
	 *  here so the Actions tab's detail pane can link into it today. */
	action: z.string().optional(),
});

export const Route = createFileRoute('/settings/actions/$tab')({
	beforeLoad: ({ params }) => {
		if (!isActionsTab(params.tab)) {
			throw redirect({ to: '/settings/actions/$tab', params: { tab: 'actions' } });
		}
	},
	validateSearch: actionsTabSearchSchema,
	component: ActionsTabRoute,
});

function ActionsTabRoute() {
	const { tab } = Route.useParams();
	return <ActionsShell tab={tab as ActionsTabId} />;
}
