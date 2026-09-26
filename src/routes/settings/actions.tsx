// /settings/actions — redirects to the default tab, same pattern as
// `/ngwa/index.tsx` redirecting to `/ngwa/installed` (WP-57).
//
// Review round 1 blocker 1: this file is the flat-route *parent* of
// `actions.$tab.tsx` (TanStack's dot-nesting — `actions.tsx` + `actions.$tab.tsx`
// makes this the layout parent), so its `beforeLoad` runs on every
// `/settings/actions/*` navigation, not only the bare parent path. An
// unconditional redirect here loops: every tab URL (`/settings/actions/menus`,
// …) would bounce straight back to `/settings/actions/$tab` with `tab: 'actions'`
// before the child route ever gets to render. Redirect only when the pathname
// is exactly the parent path; every other child path renders through the
// `<Outlet/>` to the matched `actions.$tab` route as normal.

import { createFileRoute, Outlet, redirect } from '@tanstack/react-router';

export const Route = createFileRoute('/settings/actions')({
	beforeLoad: ({ search, location }) => {
		const pathname = location.pathname;
		if (pathname !== '/settings/actions' && pathname !== '/settings/actions/') return;
		throw redirect({
			to: '/settings/actions/$tab',
			params: { tab: 'actions' },
			search,
			hash: location.hash,
		});
	},
	component: () => <Outlet />,
});
