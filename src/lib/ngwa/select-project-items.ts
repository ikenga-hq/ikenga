// DEC-74 (Round 58) — which Ngwa snapshot items belong to a given Explorer
// project scope. Shared by the "Ngwa · project" Explorer section and its
// row-count badge so they can never disagree.
//
// DEC-71: `'default'` is the shell's personal-scope sentinel
// (`useShellStore.activeProjectId`), not a real project row — it maps to
// Ngwa's own `{ kind: 'personal' }` scope (contract `ngwa.ts`), the same way
// the Scopes matrix's 'personal' column and `wireOf('personal') === 'workspace'`
// do. Any other project id maps to `{ kind: 'project', project_id }`.
//
// Decision (stated in the PR, not implied anywhere else): personal-scope
// items do NOT also appear under a real project here. D-01's static mock
// doesn't distinguish personal vs. project-scoped rows (it's placeholder
// data), so this follows the task's fallback rule and keeps a real project's
// list to that project's own items — mirroring Scopes/Installed, where
// personal and project are always separate columns/facets, never merged.

import type { NgwaItem } from '@ikenga/contract';

export function selectProjectNgwaItems(
	items: readonly NgwaItem[] | null | undefined,
	projectId: string
): NgwaItem[] {
	if (!items) return [];
	if (!projectId || projectId === 'default') {
		return items.filter((i) => i.scope.kind === 'personal');
	}
	return items.filter((i) => i.scope.kind === 'project' && i.scope.project_id === projectId);
}
