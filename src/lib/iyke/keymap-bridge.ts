// Read-only seam for WP-21's `iyke keys list`: the iyke FE layer imports
// `listKeymap` from here instead of reaching into `@/lib/keymap` directly, so
// this file — not the registry's internal shape — is the contract WP-21
// codes against. Additive only; nothing in `src/lib/iyke` is modified.

export { listKeymap, resolveKeypress, subscribeKeymap } from '@/lib/keymap/registry';
export type { KeymapEntry, KeypressResolution } from '@/lib/keymap/registry';

// WP-62: the `iyke` actions/menus/keys surface (D-06 footer lines) binds to
// G-ACTIONS-API exactly the way the D-06 UI does — through this seam, not by
// reaching into `@/lib/actions/store` directly — so the CLI and the UI share
// one code path into the one WP-50 validator (`actions-schema.md` §1.6).
export {
	ActionsValidationError,
	addKeybinding,
	getEffectiveModel,
	saveUserAction,
	subscribeEffectiveModel,
} from '@/lib/actions/store';
export type {
	ActionsScope,
	EffectiveAction,
	EffectiveMenu,
	EffectiveModel,
	KeybindingRule,
	UserAction,
} from '@/lib/actions/store';
