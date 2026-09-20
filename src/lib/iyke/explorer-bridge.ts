// Read-only seam for WP-21's `iyke explorer list`: the iyke FE layer imports
// `listExplorerSections` from here instead of reaching into the registry directly, so
// this file — not the registry's internal shape — is the contract WP-21
// codes against. Additive only; nothing in `src/lib/iyke` is modified.

export { listExplorerSections } from '@/shell/explorer/section-registry';
export type { ExplorerSectionDefinition } from '@/shell/explorer/section-registry';
