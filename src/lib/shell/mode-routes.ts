// Route → owning activity mode.
//
// The activity-bar mode (`useShellStore.activeMode`) and the focused pane's
// route are separate state. Clicking a rail icon sets the mode *and* navigates
// (see `activity-bar.tsx` MODE_LANDING); but a bare programmatic navigation —
// `/iyke/go`, a deep link — only moves the pane's route, leaving the mode
// (and therefore the rail highlight, sidebar, and workspace tint) stale.
//
// This maps a route back to the mode that *exclusively* owns it, so callers
// that navigate the focused pane can re-sync the mode. v16 (G-STATE) modes:
//   • /packages*  — Ngwa (packages moved under Ngwa)
//   • /claude*    — Ngwa (Claude-config browser)
//   • /ngwa*      — Ngwa
//   • /settings*  — Settings
//   • /chi*       — Chi
// Routes that live under Project (/, /sessions, /artifacts, /todos, /cron …)
// are intentionally absent — navigation to them leaves the current mode
// untouched rather than guess.
//
// Package routes (`/pkg/<id>/…`) return null: package views live under
// Project and no longer own a mode of their own (g-state.md §5).
//
// This is the inverse direction of `activity-bar.tsx`'s `MODE_LANDING`, kept
// here as a route *prefix* map (coarser than landing's exact paths). Keep the
// two in sync when a mode's route territory changes.

import type { CoreMode } from './shell-store';

const EXCLUSIVE_MODE_PREFIXES: ReadonlyArray<readonly [string, CoreMode]> = [
	['/packages', 'ngwa'],
	['/claude', 'ngwa'],
	['/ngwa', 'ngwa'],
	['/settings', 'settings'],
	['/chi', 'chi'],
];

/**
 * The activity mode that *exclusively* owns `path`, or `null` if the route is
 * shared (the caller should keep the current mode). Matches on the pathname
 * only, so query strings (`/packages?filter=review`) and sub-paths
 * (`/settings/appearance`) resolve to the same owning mode, while a lookalike
 * sibling (`/packages-foo`) does not.
 */
export function modeForRoute(path: string): CoreMode | null {
	const pathname = path.split(/[?#]/, 1)[0] ?? path;
	for (const [prefix, mode] of EXCLUSIVE_MODE_PREFIXES) {
		if (pathname === prefix || pathname.startsWith(`${prefix}/`)) return mode;
	}
	return null;
}
