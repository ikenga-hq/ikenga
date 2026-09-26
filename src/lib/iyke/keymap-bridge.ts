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
	getEffectiveKeymap,
	getEffectiveModel,
	saveUserAction,
	subscribeEffectiveModel,
} from '@/lib/actions/store';
export type {
	ActionsScope,
	EffectiveAction,
	EffectiveKeymap,
	EffectiveMenu,
	EffectiveModel,
	HeldKeybinding,
	KeybindingRule,
	UserAction,
} from '@/lib/actions/store';

// WP-62 review (S3): project-action trust state is not part of the merged
// `EffectiveModel` — it's WP-50's own record (`ActionsTrustStatus`), read
// through the file-layer client rather than the store. Read-only, same
// "share one code path" spirit as the store re-exports above.
export { actionsTrustStatus } from '@/lib/actions/client';
export type { ActionTrust, ActionsTrustStatus, TrustState, Validation } from '@/lib/actions/client';
