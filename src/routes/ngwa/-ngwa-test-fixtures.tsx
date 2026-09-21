// Test-only fixtures + router harness for the /ngwa/scopes and /ngwa/health
// route tests (WP-16a). The leading `-` keeps it out of the route tree.

import type { ReactNode } from 'react';
import { render } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import {
	Outlet,
	RouterProvider,
	createMemoryHistory,
	createRootRoute,
	createRouter,
	type AnyRoute,
} from '@tanstack/react-router';
import type { NgwaItem, NgwaPlacement, NgwaSnapshot } from '@ikenga/contract';

// jsdom has no scrollTo; the router's scroll restoration calls it.
if (typeof window !== 'undefined') window.scrollTo = (() => {}) as typeof window.scrollTo;

export function mkItem(p: Partial<NgwaItem> & Pick<NgwaItem, 'id' | 'kind' | 'name'>): NgwaItem {
	return {
		display_name: p.name,
		description: null,
		version: null,
		latest_version: null,
		scope: { kind: 'personal' },
		origin: {
			source: 'local',
			url: null,
			ref: null,
			resolved_version: null,
			publisher: null,
			managed: false,
			auto_update: false,
			installed_at_ms: null,
			updated_at_ms: null,
		},
		state: 'enabled',
		runtime: null,
		trust: {
			state: 'not_applicable',
			signed: false,
			auto_trusted: false,
			review_pending: false,
			perms: null,
			last_granted_at_ms: null,
		},
		placements: [],
		usage: null,
		requires: [],
		required_by: [],
		owner_pkg_id: null,
		install_path: null,
		engines: [],
		...p,
	};
}

export function mkPlacement(p: Partial<NgwaPlacement> & Pick<NgwaPlacement, 'path'>): NgwaPlacement {
	return {
		engine: 'claude',
		scope: { kind: 'personal' },
		mechanism: 'symlink-dir',
		present: true,
		link_target: null,
		in_store: false,
		managed_by: 'user',
		overridden_by: null,
		format: 'md-yaml',
		status: 'active',
		...p,
	};
}

export const P1_ROOT = '/r/royalti-co';
export const P2_ROOT = '/r/ikenga';

export function engineItems(which: Array<'claude' | 'codex' | 'gemini'>): NgwaItem[] {
	const ids = { claude: 'claude-code', codex: 'codex', gemini: 'gemini' } as const;
	return which.map((e) =>
		mkItem({
			id: `com.ikenga.engine-${ids[e]}`,
			kind: 'engine',
			name: `com.ikenga.engine-${ids[e]}`,
			version: e === 'claude' ? '0.2.1' : '0.1.0',
			origin: {
				source: e === 'claude' ? 'builtin' : 'registry',
				url: null,
				ref: null,
				resolved_version: null,
				publisher: null,
				managed: e !== 'claude',
				auto_update: false,
				installed_at_ms: 1,
				updated_at_ms: null,
			},
			trust: {
				state: e === 'claude' ? 'auto_trusted' : 'granted',
				signed: e === 'claude',
				auto_trusted: e === 'claude',
				review_pending: false,
				perms: null,
				last_granted_at_ms: null,
			},
			install_path: `/pkgs/${ids[e]}`,
		})
	);
}

/** The scopes fixture: every row shape the matrix must get right. */
export function scopesItems(): NgwaItem[] {
	return [
		...engineItems(['claude', 'codex']),
		// Ọba skill, personal store symlink, shadowed by a real project copy (DEC-31).
		// Both versions are null: detection must not depend on version.
		mkItem({
			id: 'skill:personal:groundwork',
			kind: 'skill',
			name: 'groundwork',
			install_path: '/store/skills/groundwork',
			placements: [
				mkPlacement({
					path: '/home/.claude/skills/groundwork',
					link_target: '/store/skills/groundwork',
					in_store: true,
					managed_by: 'oba',
					overridden_by: `${P1_ROOT}/.claude/skills/groundwork`,
				}),
			],
			engines: ['claude'],
		}),
		mkItem({
			id: 'skill:project:p1:groundwork',
			kind: 'skill',
			name: 'groundwork',
			scope: { kind: 'project', project_id: 'p1' },
			placements: [
				mkPlacement({
					path: `${P1_ROOT}/.claude/skills/groundwork`,
					scope: { kind: 'project', project_id: 'p1' },
				}),
			],
			engines: ['claude'],
		}),
		// Same name, different kind, in another project: NOT a conflict.
		mkItem({
			id: 'agent:project:p2:groundwork',
			kind: 'agent',
			name: 'groundwork',
			scope: { kind: 'project', project_id: 'p2' },
			placements: [
				mkPlacement({
					path: `${P2_ROOT}/.claude/agents/groundwork.md`,
					mechanism: 'file',
					scope: { kind: 'project', project_id: 'p2' },
				}),
			],
			engines: ['claude'],
		}),
		// Ọba agent in the store, not placed in personal (cell = off).
		mkItem({
			id: 'agent:personal:explore',
			kind: 'agent',
			name: 'explore',
			install_path: '/store/agents/explore.md',
			state: 'disabled',
		}),
		// Personal store-backed skill placed for claude (personal) and codex (in p2):
		// the engine cells read the union.
		mkItem({
			id: 'skill:personal:lint',
			kind: 'skill',
			name: 'lint',
			version: '1.2.0',
			install_path: '/store/skills/lint',
			placements: [
				mkPlacement({
					path: '/home/.claude/skills/lint',
					link_target: '/store/skills/lint',
					in_store: true,
				}),
			],
			engines: ['claude'],
		}),
		mkItem({
			id: 'skill:project:p2:lint',
			kind: 'skill',
			name: 'lint',
			scope: { kind: 'project', project_id: 'p2' },
			placements: [
				mkPlacement({
					engine: 'codex',
					path: `${P2_ROOT}/.agents/skills/lint`,
					scope: { kind: 'project', project_id: 'p2' },
					link_target: '/store/skills/lint',
					in_store: true,
				}),
			],
			engines: ['codex'],
		}),
		// A real personal command file (not in the store).
		mkItem({
			id: 'command:personal:release',
			kind: 'command',
			name: 'release',
			placements: [mkPlacement({ path: '/home/.claude/commands/release.md', mechanism: 'file' })],
			engines: ['claude'],
		}),
		// Pkgs.
		mkItem({
			id: 'com.ikenga.tasks',
			kind: 'app',
			name: 'com.ikenga.tasks',
			display_name: 'Tasks',
			version: '0.8.2',
			install_path: '/pkgs/tasks',
			origin: {
				source: 'registry',
				url: null,
				ref: null,
				resolved_version: null,
				publisher: null,
				managed: true,
				auto_update: true,
				installed_at_ms: 1,
				updated_at_ms: null,
			},
		}),
		mkItem({
			id: 'com.ikenga.studio',
			kind: 'app',
			name: 'com.ikenga.studio',
			display_name: 'Studio',
			scope: { kind: 'project', project_id: 'p1' },
			state: 'disabled',
			install_path: '/pkgs/studio',
		}),
	];
}

export function mkSnapshot(items: NgwaItem[], over: Partial<NgwaSnapshot> = {}): NgwaSnapshot {
	const ok = { ok: true, error: null, count: 1 };
	return {
		items,
		as_of_ms: Date.now(),
		sources: {
			kernel: ok,
			oba: ok,
			engine_config: ok,
			engine_assets: ok,
			trust: ok,
			usage: ok,
		},
		...over,
	};
}

export const PROJECTS = [
	{
		id: 'p1',
		display_name: 'royalti-co',
		root_path: P1_ROOT,
		icon: null,
		color: null,
		description: null,
		position: 1,
		is_default: false,
		created_at: 1,
		archived_at: null,
	},
	{
		id: 'p2',
		display_name: 'ikenga',
		root_path: P2_ROOT,
		icon: null,
		color: null,
		description: null,
		position: 0,
		is_default: false,
		created_at: 1,
		archived_at: null,
	},
];

/** Mount real file routes under a bare root at `initial`. */
export function mountRoutes(routes: Array<{ route: AnyRoute; path: string }>, initial: string) {
	const root = createRootRoute({ component: () => <Outlet /> });
	const children = routes.map(({ route, path }) =>
		// biome-ignore lint/suspicious/noExplicitAny: same shape routeTree.gen.ts uses
		(route as any).update({ id: path, path, getParentRoute: () => root })
	);
	const router = createRouter({
		routeTree: root.addChildren(children),
		history: createMemoryHistory({ initialEntries: [initial] }),
	});
	const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	const wrap = (ui: ReactNode) => <QueryClientProvider client={qc}>{ui}</QueryClientProvider>;
	const utils = render(wrap(<RouterProvider router={router} />));
	return { router, qc, ...utils };
}
