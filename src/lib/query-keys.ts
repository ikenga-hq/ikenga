/**
 * Central query key factory for TanStack Query.
 *
 * Post-strip: only shell-internal queries live here. App pkgs (email, finance,
 * etc.) keep their own factories inside their own iframe bundles and namespace
 * via the QueriesRegistry's `queries.key_prefixes` declaration.
 *
 * Convention: each domain returns a flat object of factories. Always include
 * the domain name as the first segment so cross-domain invalidations are easy.
 */

export const queryKeys = {
	sessions: {
		all: ['claude_sessions'] as const,
		list: (projectDir?: string | null) => ['claude_sessions', 'list', projectDir ?? 'all'] as const,
		detail: (sessionId: string) => ['claude_sessions', 'detail', sessionId] as const,
	},
	// WP-40 notification aggregation table. Every mutation and every
	// `notifications://changed` event invalidates `all`.
	notifications: {
		all: ['notifications'] as const,
		list: (filter: {
			unreadOnly?: boolean;
			kinds?: readonly string[];
			limit?: number;
			includeMuted?: boolean;
		} = {}) =>
			[
				'notifications',
				'list',
				filter.unreadOnly ?? false,
				[...(filter.kinds ?? [])].sort().join(',') || 'all',
				filter.limit ?? null,
				filter.includeMuted ?? false,
			] as const,
		unreadCount: () => ['notifications', 'unread-count'] as const,
		muteState: () => ['notifications', 'mute-state'] as const,
	},
	secrets: {
		all: ['secrets'] as const,
		vaultStatus: () => ['secrets', 'vault-status'] as const,
		lockState: () => ['secrets', 'lock-state'] as const,
		keys: () => ['secrets', 'keys'] as const,
	},
	fs: {
		all: ['fs'] as const,
		list: (path: string) => ['fs', 'list', path] as const,
		search: (root: string, query: string, showHidden: boolean, showIgnored: boolean) =>
			['fs', 'search', root, query, showHidden, showIgnored] as const,
	},
	claudeConfig: {
		all: ['claude_config'] as const,
		load: (projectRoots: readonly string[]) =>
			['claude_config', 'load', [...projectRoots].sort().join('|')] as const,
	},
	// Ngwa central store (Ọba) catalog. Distinct domain from `claudeConfig`
	// (the on-disk scan) — store mutations invalidate BOTH: the catalog list
	// (enabledIn badges) and the scan (the symlink/merge state on disk, which
	// rides the existing `claude-config:changed` watch).
	claudeStore: {
		all: ['claude_store'] as const,
		list: (kind?: string | null) => ['claude_store', 'list', kind ?? 'all'] as const,
	},
	// Approve-gate run-then-pause draft queue (pa_action_drafts). The route at
	// /outbox/approvals reads it; commit/reject mutations invalidate `all`.
	paActions: {
		all: ['pa_actions'] as const,
		list: (status?: string | null) => ['pa_actions', 'list', status ?? 'active'] as const,
	},
	// Every installed skill's actions (list_all_skill_actions), backing the ⌘K
	// palette Actions group. No pkg-install Tauri event exists to invalidate on,
	// so the hook leans on a ~30s staleTime (WP-18a).
	skillActions: {
		all: ['skill_actions', 'all'] as const,
	},
	// WP-18c — Obi home live widgets. Direct `db_query` reads against the
	// domain tables the mock widgets used to fake (tasks / mail / finance).
	// These are shell-side reads (not the pkg-iframe `host.dbQuery` bridge), so
	// no `sqlite.tables` capability check applies — see `lib/queries/home-widgets.ts`.
	home: {
		tasks: () => ['home', 'tasks'] as const,
		inbox: () => ['home', 'inbox'] as const,
		finance: () => ['home', 'finance'] as const,
		boardsPkgStatus: () => ['home', 'boards', 'pkg-status'] as const,
	},
} as const;
