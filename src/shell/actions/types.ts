// D-06 tab id (WP-57) — shared by the shell, the header, the tab bar and the
// `/settings/actions/$tab` route, so none of them has to import from the
// route file (routes depend on shell code, not the other way around).

export const ACTIONS_TABS = ['actions', 'editor', 'menus', 'keys'] as const;
export type ActionsTabId = (typeof ACTIONS_TABS)[number];

export function isActionsTab(value: string): value is ActionsTabId {
	return (ACTIONS_TABS as readonly string[]).includes(value);
}
