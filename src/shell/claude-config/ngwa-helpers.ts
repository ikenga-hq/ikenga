import type {
	ClaudeAgent,
	ClaudeCommand,
	ClaudeConfig,
	ClaudeHook,
	ClaudeMcp,
	ClaudeSkill,
	ClaudeStoreEntry,
	ClaudeStoreKind,
	ClaudeStoreScope,
	Project,
} from '@/lib/tauri-cmd';
import type { ConfigFormat, EngineId, KindStatus } from '@/lib/queries/claude-config';

export type NgwaSurfaceId =
	| 'browse'
	| 'registry'
	| 'store'
	| 'graph'
	| 'map'
	| 'life'
	| 'health'
	| 'flow';

export type NgwaScopeId = 'all' | 'personal' | `project:${string}`;
export type NgwaKindId = 'skills' | 'agents' | 'commands' | 'hooks' | 'mcps';
export type NgwaSystemId = EngineId;

export const ENGINE_ORDER: readonly NgwaSystemId[] = ['claude', 'gemini', 'codex'];

export const ENGINE_META: Record<
	NgwaSystemId,
	{ display: string; badge: string; code: 'cl' | 'gm' | 'cx' }
> = {
	claude: { display: 'Claude Code', badge: 'CL', code: 'cl' },
	gemini: { display: 'Gemini', badge: 'GM', code: 'gm' },
	codex: { display: 'Codex', badge: 'CX', code: 'cx' },
};

export const systemOf = (e: { system?: EngineId }): NgwaSystemId => e.system ?? 'claude';
export const statusOf = (e: { status?: KindStatus }): KindStatus => e.status ?? 'active';

export type ItemState = 'enabled' | 'disabled' | 'local' | 'orphaned' | 'linked';
export type ItemMech = 'link' | 'merge';

export interface NgwaItem {
	id: string;
	storeKind: ClaudeStoreKind;
	uiKind: NgwaKindId;
	name: string;
	scope: 'personal' | 'project';
	scopeKey: ClaudeStoreScope;
	scopeLabel: string;
	projectRoot: string | null;
	path: string;
	description: string | null;
	state: ItemState;
	mech: ItemMech;
	overriddenBy: string | null;
	system: NgwaSystemId;
	format: ConfigFormat | null;
	status: KindStatus;
	raw: ClaudeAgent | ClaudeSkill | ClaudeCommand | ClaudeHook | ClaudeMcp;
	storeEntry: ClaudeStoreEntry | null;
}

export const UI_KIND_OF: Record<ClaudeStoreKind, NgwaKindId> = {
	skill: 'skills',
	agent: 'agents',
	command: 'commands',
	hook: 'hooks',
	mcp: 'mcps',
	bundle: 'skills',
};

export const normRoot = (p: string) => p.replace(/\/+$/, '');
export const baseOf = (p: string) => normRoot(p).split('/').filter(Boolean).pop() ?? '';

export function projectIdForRoot(projects: Project[], root: string | null): string | null {
	if (!root) return null;
	const r = normRoot(root);
	const exact = projects.find((p) => p.root_path && normRoot(p.root_path) === r);
	if (exact) return exact.id;
	const rb = baseOf(root);
	const byBase = projects.find((p) => p.root_path && baseOf(p.root_path) === rb);
	return byBase?.id ?? null;
}

export function scopeKeyOf(
	scope: 'personal' | 'project',
	projectRoot: string | null,
	projects: Project[]
): ClaudeStoreScope {
	if (scope === 'personal') return 'workspace';
	const id = projectIdForRoot(projects, projectRoot) ?? baseOf(projectRoot ?? '') ?? 'project';
	return `project:${id || 'project'}`;
}

export function scopeLabelOf(scope: 'personal' | 'project', projectRoot: string | null): string {
	if (scope === 'personal') return 'Personal';
	return (projectRoot ?? 'project').split('/').filter(Boolean).pop() ?? 'project';
}

export function deriveState(
	meta: { isSymlink: boolean; inStore: boolean; targetExists?: boolean },
	mech: ItemMech
): ItemState {
	if (mech === 'merge') return 'enabled';
	if (meta.isSymlink && !meta.inStore) return 'linked';
	if (meta.isSymlink && meta.inStore) {
		if (meta.targetExists === false) return 'orphaned';
		return 'enabled';
	}
	return 'local';
}

export function buildItems(
	config: ClaudeConfig,
	store: ClaudeStoreEntry[],
	projects: Project[]
): NgwaItem[] {
	const out: NgwaItem[] = [];
	const storeByKey = new Map<string, ClaudeStoreEntry>();
	for (const e of store) storeByKey.set(`${e.kind}:${e.name}`, e);
	const seen = new Map<string, number>();

	const push = (
		storeKind: ClaudeStoreKind,
		name: string,
		scope: 'personal' | 'project',
		projectRoot: string | null,
		path: string,
		description: string | null,
		mech: ItemMech,
		meta: { isSymlink: boolean; inStore: boolean },
		overriddenBy: string | null,
		raw: NgwaItem['raw']
	) => {
		const scopeKey = scopeKeyOf(scope, projectRoot, projects);
		const storeEntry = storeByKey.get(`${storeKind}:${name}`) ?? null;
		const system = systemOf(raw);
		let id = `${system}:${storeKind}:${name}:${scope}:${projectRoot ?? ''}`;
		const dup = seen.get(id) ?? 0;
		seen.set(id, dup + 1);
		if (dup > 0) id = `${id}#${dup}`;
		out.push({
			id,
			storeKind,
			uiKind: UI_KIND_OF[storeKind],
			name,
			scope,
			scopeKey,
			scopeLabel: scopeLabelOf(scope, projectRoot),
			projectRoot,
			path,
			description,
			state: deriveState(meta, mech),
			mech,
			overriddenBy,
			system,
			format: raw.format ?? null,
			status: statusOf(raw),
			raw,
			storeEntry,
		});
	};

	for (const a of config.agents)
		push(
			'agent',
			a.name,
			a.scope,
			a.projectRoot,
			a.path,
			a.description,
			'link',
			a,
			a.overriddenBy,
			a
		);
	for (const s of config.skills)
		push(
			'skill',
			s.name,
			s.scope,
			s.projectRoot,
			s.path,
			s.description,
			'link',
			s,
			s.overriddenBy,
			s
		);
	for (const c of config.commands)
		push(
			'command',
			c.name,
			c.scope,
			c.projectRoot,
			c.path,
			c.description,
			'link',
			c,
			c.overriddenBy,
			c
		);
	for (const h of config.hooks)
		push(
			'hook',
			h.name,
			h.scope,
			h.projectRoot,
			h.settingsPath,
			h.event,
			'merge',
			{ isSymlink: false, inStore: false },
			null,
			h
		);
	for (const m of config.mcps)
		push(
			'mcp',
			m.name,
			m.scope,
			m.projectRoot,
			m.path,
			`${m.transport} server`,
			'merge',
			{ isSymlink: false, inStore: false },
			null,
			m
		);

	return out;
}

export interface NgwaSystemSummary {
	present: NgwaSystemId[];
	engineCounts: Record<NgwaSystemId, number>;
	kindCounts: (active: ReadonlySet<NgwaSystemId>) => Record<NgwaKindId, number>;
}

export function summarizeSystems(items: NgwaItem[]): NgwaSystemSummary {
	const engineCounts = { claude: 0, gemini: 0, codex: 0 } as Record<NgwaSystemId, number>;
	for (const it of items) engineCounts[it.system] = (engineCounts[it.system] ?? 0) + 1;
	const present = ENGINE_ORDER.filter((e) => engineCounts[e] > 0);
	const kindCounts = (active: ReadonlySet<NgwaSystemId>): Record<NgwaKindId, number> => {
		const c: Record<NgwaKindId, number> = {
			skills: 0,
			agents: 0,
			commands: 0,
			hooks: 0,
			mcps: 0,
		};
		for (const it of items) {
			if (active.has(it.system)) c[it.uiKind] += 1;
		}
		return c;
	};
	return { present, engineCounts, kindCounts };
}

export function siblingSystemsOf(item: NgwaItem | null, items: NgwaItem[]): NgwaSystemId[] {
	if (!item) return [];
	const others = new Set<NgwaSystemId>();
	for (const x of items) {
		if (x.storeKind === item.storeKind && x.name === item.name && x.system !== item.system) {
			others.add(x.system);
		}
	}
	return ENGINE_ORDER.filter((e) => others.has(e));
}

export function resolveActiveSystems(
	selected: readonly NgwaSystemId[] | null,
	present: readonly NgwaSystemId[]
): Set<NgwaSystemId> {
	const presentSet = new Set(present);
	if (!selected || selected.length === 0) return new Set(present);
	const picked = selected.filter((s) => presentSet.has(s));
	return picked.length > 0 ? new Set(picked) : new Set(present);
}
