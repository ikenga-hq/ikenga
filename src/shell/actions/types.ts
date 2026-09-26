// D-06 tab id (WP-57) — shared by the shell, the header, the tab bar and the
// `/settings/actions/$tab` route, so none of them has to import from the
// route file (routes depend on shell code, not the other way around).
//
// `import` is routable (`/settings/actions/import`) but is not one of the
// four keys-1-4 tabs shown in the tab bar (item 19, wave 12e mount
// contract) — it is reached from the header's `⋯` "Import actions…" only.
// WP-61 replaces `src/shell/actions/import/index.tsx` with the real surface.

import type { ActionsScope, EffectiveModel } from '@/lib/actions/store';

export const ACTIONS_TABS = ['actions', 'editor', 'menus', 'keys', 'import'] as const;
export type ActionsTabId = (typeof ACTIONS_TABS)[number];

/** The four tabs shown in the 1–4 tab bar — `import` is routable but not a
 *  visible tab (reached from the header's `⋯` menu instead). */
export const ACTIONS_VISIBLE_TABS = ['actions', 'editor', 'menus', 'keys'] as const;
export type VisibleActionsTabId = (typeof ACTIONS_VISIBLE_TABS)[number];

export function isActionsTab(value: string): value is ActionsTabId {
	return (ACTIONS_TABS as readonly string[]).includes(value);
}

export function isVisibleActionsTab(tab: ActionsTabId): tab is VisibleActionsTabId {
	return (ACTIONS_VISIBLE_TABS as readonly string[]).includes(tab);
}

// ─── Wave 12e mount contract (item 19) ──────────────────────────────────────
//
// WP-58 (Editor), WP-59 (Menus), WP-60 (Keys) and WP-61 (Import) each own one
// folder under `src/shell/actions/{editor,menus,keys,import}/` and replace
// only that folder's `index.tsx` — so four parallel PRs never collide on the
// same file. Every surface takes this one prop shape; `shell.tsx` mounts the
// right surface by tab and supplies these props from the frozen G-ACTIONS-API
// model it already reads.

export interface ActionsSurfaceProps {
	scope: ActionsScope;
	model: EffectiveModel;
	onNavigate: (tab: ActionsTabId, opts?: { action?: string }) => void;
}
