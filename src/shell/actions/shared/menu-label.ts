// D-06 shared helper (WP-57): a menu id (G-ACTIONS §1.3 — `files`,
// `section/<sectionId>`, `native/<top>`, …) to a short display label, for the
// Actions tab's Placement facet/chips and the detail pane's placement list.
// Not authoritative — `menus.ts` / the effective model own real menu
// contents; this only labels an id for display.

const MENU_LABELS: Readonly<Record<string, string>> = {
	files: 'Files',
	'files-view': 'Files · view options',
	artifacts: 'Artifacts',
	session: 'Session',
	automations: 'Automations',
	'ngwa-project': 'Ngwa (project)',
	scratchpads: 'Scratchpads',
	todos: 'Todos',
	views: 'Views',
	tab: 'Tab',
	address: 'Address bar',
	pane: 'Pane ⋯',
	'viewer-frame': 'Viewer frame',
	status: 'Status bar',
	rail: 'Rail',
	'rail-section': 'Rail section',
	'rail-ngwa': 'Rail · Ngwa',
	palette: 'Command palette',
};

/** The category a menu id groups under for the Placement facet — the first
 *  path segment of a parameterized id (`section/automations` → `section`,
 *  `native/file` → `native`), or the id itself. */
export function placementCategory(menuId: string): string {
	const at = menuId.indexOf('/');
	return at < 0 ? menuId : menuId.slice(0, at);
}

/** A short label for a full menu id, for chips and the detail pane. */
export function menuLabel(menuId: string): string {
	if (MENU_LABELS[menuId]) return MENU_LABELS[menuId];
	const at = menuId.indexOf('/');
	if (at < 0) return menuId;
	const category = menuId.slice(0, at);
	const rest = menuId.slice(at + 1);
	if (category === 'section') return `Section · ${rest}`;
	if (category === 'native') return `Native · ${rest}`;
	return menuId;
}

/** A short label for a placement category (the Placement facet's chips). */
export function placementCategoryLabel(category: string): string {
	return MENU_LABELS[category] ?? (category.charAt(0).toUpperCase() + category.slice(1));
}
