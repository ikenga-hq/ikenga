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

// Paths follow the golden snapshot (`src/lib/ngwa/__fixtures__/
// ngwa-snapshot.golden.json`): home `/home/x`, a project root with a drive
// letter, skill placements ending in `/SKILL.md`, and `mechanism` being the
// layout's intent (a real skill folder still reads `symlink-dir`).
export const HOME = '/home/x';
export const STORE = '/home/x/.ikenga/store';
export const P1_ROOT = 'C:/Users/x/royalti-co';
export const P2_ROOT = 'C:/Users/x/ikenga';

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

const P1 = { kind: 'project', project_id: 'p1' } as const;
const P2 = { kind: 'project', project_id: 'p2' } as const;

/** A store-linked skill placement, golden shape. */
function skillLink(root: string, dotdir: string, name: string, extra: Partial<NgwaPlacement> = {}) {
	return mkPlacement({
		path: `${root}/${dotdir}/skills/${name}/SKILL.md`,
		link_target: `${STORE}/skills/${name}`,
		in_store: true,
		managed_by: 'oba',
		...extra,
	});
}

/** The scopes fixture: every row shape the matrix must get right. */
export function scopesItems(): NgwaItem[] {
	return [
		...engineItems(['claude', 'codex']),
		// Ọba skill: personal store link shadowed by the project copy (DEC-31,
		// golden `groundwork`). Versions are null: detection must not use them.
		mkItem({
			id: 'skill:personal:groundwork',
			kind: 'skill',
			name: 'groundwork',
			install_path: `${STORE}/skills/groundwork`,
			placements: [
				skillLink(HOME, '.claude', 'groundwork', {
					overridden_by: `${P1_ROOT}/.claude/skills/groundwork/SKILL.md`,
				}),
			],
			engines: ['claude'],
		}),
		mkItem({
			id: 'skill:project:p1:groundwork',
			kind: 'skill',
			name: 'groundwork',
			scope: P1,
			placements: [skillLink(P1_ROOT, '.claude', 'groundwork', { scope: P1 })],
			engines: ['claude'],
		}),
		// Same name, different kind, in another project: NOT a conflict.
		mkItem({
			id: 'agent:project:p2:groundwork',
			kind: 'agent',
			name: 'groundwork',
			scope: P2,
			placements: [
				mkPlacement({ path: `${P2_ROOT}/.claude/agents/groundwork.md`, mechanism: 'file', scope: P2 }),
			],
			engines: ['claude'],
		}),
		// Ọba agent in the store, placed nowhere (cell = off).
		mkItem({
			id: 'agent:personal:explore',
			kind: 'agent',
			name: 'explore',
			install_path: `${STORE}/agents/explore.md`,
			state: 'disabled',
		}),
		// Ọba agent in the store; the project holds a REAL hand-written file of
		// the same name. Enabling into p1 would replace it (must-fix 2b).
		mkItem({
			id: 'agent:personal:reviewer',
			kind: 'agent',
			name: 'reviewer',
			install_path: `${STORE}/agents/reviewer.md`,
			placements: [
				mkPlacement({
					path: `${HOME}/.claude/agents/reviewer.md`,
					link_target: `${STORE}/agents/reviewer.md`,
					in_store: true,
					managed_by: 'oba',
				}),
			],
			engines: ['claude'],
		}),
		mkItem({
			id: 'agent:project:p1:reviewer',
			kind: 'agent',
			name: 'reviewer',
			scope: P1,
			placements: [
				mkPlacement({ path: `${P1_ROOT}/.claude/agents/reviewer.md`, mechanism: 'file', scope: P1 }),
			],
			engines: ['claude'],
		}),
		// Store skill linked for claude (personal) and codex (p2, at the exact
		// `.agents/skills/` path disable_for_core deletes).
		mkItem({
			id: 'skill:personal:lint',
			kind: 'skill',
			name: 'lint',
			version: '1.2.0',
			install_path: `${STORE}/skills/lint`,
			placements: [skillLink(HOME, '.claude', 'lint')],
			engines: ['claude'],
		}),
		mkItem({
			id: 'skill:project:p2:lint',
			kind: 'skill',
			name: 'lint',
			scope: P2,
			placements: [skillLink(P2_ROOT, '.agents', 'lint', { engine: 'codex', scope: P2 })],
			engines: ['codex'],
		}),
		// A REAL skill folder (golden `com-ikenga-iyke` shape: symlink-dir, no
		// link, not in store), and a real folder under codex's `.agents/skills`.
		mkItem({
			id: 'skill:personal:notes',
			kind: 'skill',
			name: 'notes',
			placements: [
				mkPlacement({ path: `${HOME}/.claude/skills/notes/SKILL.md` }),
				mkPlacement({ engine: 'codex', path: `${HOME}/.agents/skills/notes/SKILL.md` }),
			],
			engines: ['claude', 'codex'],
		}),
		// Store-linked for codex, but NOT at the path the codex disable deletes.
		mkItem({
			id: 'skill:personal:deck',
			kind: 'skill',
			name: 'deck',
			install_path: `${STORE}/skills/deck`,
			placements: [skillLink(HOME, '.codex', 'deck', { engine: 'codex' })],
			engines: ['codex'],
		}),
		// A real personal command file, not in the store (golden `ship` shape).
		mkItem({
			id: 'command:personal:release',
			kind: 'command',
			name: 'release',
			placements: [mkPlacement({ path: `${HOME}/.claude/commands/release.md`, mechanism: 'file' })],
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
			scope: P1,
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
