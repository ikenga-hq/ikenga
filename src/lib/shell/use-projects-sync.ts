// Phase 0 project-switch subscriber. Listens for the Rust-side
// `projects:active-changed` Tauri event and:
//
//   1. Invalidates every TanStack Query whose key starts with
//      `'project-scoped'`. Phase 0 doesn't define those queries yet — this
//      establishes the convention. Later phases (sessions, pkgs, todos,
//      cron) opt in by prefixing their keys with `'project-scoped'`.
//
//   2. Refreshes the in-memory project list from Rust so the activity-bar
//      indicator stays in sync after MCP-driven switches (the Iyke MCP
//      can call `project_set_active` without going through the shell store).
//
// Mounted at workspace level — exactly once per process.

import { useEffect } from 'react';

import { queryClient } from '@/lib/query-client';
import { useShellStore } from '@/lib/shell/shell-store';
import { projectListenActiveChanged, type UnlistenFn } from '@/lib/tauri-cmd';

export function useProjectsSync(): void {
	const refreshProjects = useShellStore((s) => s.refreshProjects);

	useEffect(() => {
		let unlisten: UnlistenFn | null = null;
		let cancelled = false;
		void (async () => {
			try {
				const fn = await projectListenActiveChanged(() => {
					// Pull the fresh list so an MCP-driven switch shows up in
					// the activity-bar indicator immediately.
					void refreshProjects();
					// Invalidate every project-scoped query in flight.
					queryClient.invalidateQueries({ queryKey: ['project-scoped'] });
				});
				if (cancelled) {
					fn();
					return;
				}
				unlisten = fn;
			} catch {
				// Tauri unavailable (test env / pre-setup boot).
			}
		})();
		return () => {
			cancelled = true;
			if (unlisten) unlisten();
		};
	}, [refreshProjects]);
}
