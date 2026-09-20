// Read-only seam for WP-21's `iyke keys list`: the iyke FE layer imports
// `listKeymap` from here instead of reaching into `@/lib/keymap` directly, so
// this file — not the registry's internal shape — is the contract WP-21
// codes against. Additive only; nothing in `src/lib/iyke` is modified.

export { listKeymap } from '@/lib/keymap/registry';
export type { KeymapEntry } from '@/lib/keymap/registry';
