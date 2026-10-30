// Project subscribers. Listens for two Rust-side Tauri events:
//
// `projects:active-changed` (a switch):
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
// `projects:list-changed` (a create / update / archive, from any surface):
//
//   Refreshes the list and nothing else. A new project must not invalidate
//   `project-scoped` queries — the active project did not change — so this
//   handler deliberately shares no code with the one above.
//
// Mounted at workspace level — exactly once per process.

import { useEffect } from 'react';
import type { UnlistenFn } from '@/lib/transport';

import { queryClient } from '@/lib/query-client';
import { useShellStore } from '@/lib/shell/shell-store';
import { projectListenActiveChanged, projectListenListChanged } from '@/lib/tauri-cmd';

export function useProjectsSync(): void {
	const refreshProjects = useShellStore((s) => s.refreshProjects);

	useEffect(() => {
		const unlisteners: UnlistenFn[] = [];
		let cancelled = false;

		// Registers one listener; if the effect was torn down while the
		// registration was in flight, unlistens immediately.
		const register = async (subscribe: () => Promise<UnlistenFn>) => {
			try {
				const fn = await subscribe();
				if (cancelled) {
					fn();
					return;
				}
				unlisteners.push(fn);
			} catch {
				// Tauri unavailable (test env / pre-setup boot).
			}
		};

		void register(() =>
			projectListenActiveChanged(() => {
				// Pull the fresh list so an MCP-driven switch shows up in
				// the activity-bar indicator immediately.
				void refreshProjects();
				// Invalidate every project-scoped query in flight.
				queryClient.invalidateQueries({ queryKey: ['project-scoped'] });
			})
		);
		void register(() =>
			projectListenListChanged(() => {
				void refreshProjects();
			})
		);

		return () => {
			cancelled = true;
			for (const fn of unlisteners) fn();
		};
	}, [refreshProjects]);
}
