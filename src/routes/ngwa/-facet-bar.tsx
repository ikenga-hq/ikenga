// In-route facet bar for Ngwa (WP-10).
//
// Replaces the deleted sidebar facets from ngwa-mode.tsx. Exposes:
//   1. Surface: Manage (browse, registry, store) & Analyze (graph, map, life, health, flow)
//   2. Scope: all, personal, project:<id>
//   3. System: multi-select engines (claude, gemini, codex)
//   4. Kind: skills, agents, commands, hooks, mcps
//
// Sets the identical search params (?surface=, ?scope=, ?kind=, ?sys=)
// that ngwa-mode.tsx previously threaded into the route.

import { useNavigate } from '@tanstack/react-router';
import { useMemo } from 'react';
import { useShellStore } from '@/lib/shell/shell-store';
import { cn } from '@/components/ui/utils';
import { Check } from 'lucide-react';

export type NgwaSurfaceId = 'browse' | 'registry' | 'store' | 'graph' | 'map' | 'life' | 'health' | 'flow';
export type NgwaKindId = 'skills' | 'agents' | 'commands' | 'hooks' | 'mcps';
export type NgwaSystemId = 'claude' | 'gemini' | 'codex';

export interface NgwaSearchParams {
	surface?: NgwaSurfaceId;
	scope?: string;
	kind?: NgwaKindId;
	sys?: string;
	install?: string;
}

const MANAGE_SURFACES: readonly { id: NgwaSurfaceId; label: string }[] = [
	{ id: 'browse', label: 'Browse' },
	{ id: 'registry', label: 'Registry' },
	{ id: 'store', label: 'Store' },
];

const ANALYZE_SURFACES: readonly { id: NgwaSurfaceId; label: string }[] = [
	{ id: 'graph', label: 'Graph' },
	{ id: 'map', label: 'Map' },
	{ id: 'life', label: 'Life' },
	{ id: 'health', label: 'Health' },
	{ id: 'flow', label: 'Flow' },
];

const KINDS: readonly { id: NgwaKindId; label: string }[] = [
	{ id: 'skills', label: 'Skills' },
	{ id: 'agents', label: 'Agents' },
	{ id: 'commands', label: 'Commands' },
	{ id: 'hooks', label: 'Hooks' },
	{ id: 'mcps', label: 'MCPs' },
];

const SYSTEMS: readonly { id: NgwaSystemId; label: string }[] = [
	{ id: 'claude', label: 'Claude' },
	{ id: 'gemini', label: 'Gemini' },
	{ id: 'codex', label: 'Codex' },
];

export function NgwaFacetBar({
	search,
	onSearchChange,
}: {
	search: NgwaSearchParams;
	onSearchChange?: (next: NgwaSearchParams) => void;
}) {
	const navigate = useNavigate();
	const activeProject = useShellStore((s) => s.activeProject);

	const activeSurface: NgwaSurfaceId = search.surface ?? 'browse';
	const activeScope = search.scope ?? 'all';
	const activeKind: NgwaKindId = search.kind ?? 'skills';

	const activeSystems: NgwaSystemId[] = useMemo(() => {
		if (!search.sys) return ['claude', 'gemini', 'codex'];
		return search.sys
			.split(',')
			.map((s) => s.trim())
			.filter((s): s is NgwaSystemId => ['claude', 'gemini', 'codex'].includes(s));
	}, [search.sys]);

	const isAnalyze = ANALYZE_SURFACES.some((s) => s.id === activeSurface);

	function updateSearch(patch: Partial<NgwaSearchParams>) {
		const next: NgwaSearchParams = {
			...search,
			...patch,
		};
		if (onSearchChange) {
			onSearchChange(next);
		} else {
			void navigate({
				search: next as any,
			});
		}
	}

	function toggleSystem(engine: NgwaSystemId) {
		const has = activeSystems.includes(engine);
		let nextSystems: NgwaSystemId[];
		if (has) {
			nextSystems = activeSystems.filter((s) => s !== engine);
		} else {
			nextSystems = [...activeSystems, engine];
		}
		// If all selected or none, serialize cleanly
		const sysParam = nextSystems.length === 3 ? undefined : nextSystems.join(',');
		updateSearch({ sys: sysParam });
	}

	function setAllSystems(all: boolean) {
		updateSearch({ sys: all ? undefined : '' });
	}

	return (
		<div className="border-b border-border bg-card/60 px-4 py-2.5 backdrop-blur-xs flex flex-wrap items-center gap-y-2 gap-x-4 text-xs">
			{/* ── Surface toggle ── */}
			<div className="flex items-center gap-1.5" role="group" aria-label="Surface">
				<span className="font-semibold text-muted-foreground uppercase text-[10px] tracking-wider mr-1">
					Surface
				</span>
				<div className="inline-flex rounded-md bg-muted p-0.5">
					{MANAGE_SURFACES.map((s) => (
						<button
							key={s.id}
							type="button"
							data-surface={s.id}
							aria-pressed={activeSurface === s.id}
							onClick={() => updateSearch({ surface: s.id })}
							className={cn(
								'rounded px-2.5 py-1 font-medium transition-colors',
								activeSurface === s.id
									? 'bg-background text-foreground shadow-xs'
									: 'text-muted-foreground hover:text-foreground'
							)}
						>
							{s.label}
						</button>
					))}
					<div className="mx-1 my-1 w-px bg-border/60" />
					{ANALYZE_SURFACES.map((s) => (
						<button
							key={s.id}
							type="button"
							data-surface={s.id}
							aria-pressed={activeSurface === s.id}
							onClick={() => updateSearch({ surface: s.id })}
							className={cn(
								'rounded px-2 py-1 font-medium transition-colors text-[11px]',
								activeSurface === s.id
									? 'bg-background text-foreground shadow-xs'
									: 'text-muted-foreground hover:text-foreground'
							)}
						>
							{s.label}
						</button>
					))}
				</div>
			</div>

			<div className="h-4 w-px bg-border hidden sm:block" />

			{/* ── Scope selector ── */}
			<div className="flex items-center gap-1.5" role="group" aria-label="Scope">
				<span className="font-semibold text-muted-foreground uppercase text-[10px] tracking-wider mr-1">
					Scope
				</span>
				<div className="inline-flex rounded-md bg-muted p-0.5">
					<button
						type="button"
						data-scope="all"
						aria-pressed={activeScope === 'all'}
						onClick={() => updateSearch({ scope: 'all' })}
						className={cn(
							'rounded px-2 py-1 font-medium transition-colors',
							activeScope === 'all'
								? 'bg-background text-foreground shadow-xs'
								: 'text-muted-foreground hover:text-foreground'
						)}
					>
						All
					</button>
					<button
						type="button"
						data-scope="personal"
						aria-pressed={activeScope === 'personal'}
						onClick={() => updateSearch({ scope: 'personal' })}
						className={cn(
							'rounded px-2 py-1 font-medium transition-colors',
							activeScope === 'personal'
								? 'bg-background text-foreground shadow-xs'
								: 'text-muted-foreground hover:text-foreground'
						)}
					>
						Personal
					</button>
					{activeProject && (
						<button
							type="button"
							data-scope={`project:${activeProject.id}`}
							aria-pressed={activeScope === `project:${activeProject.id}`}
							onClick={() => updateSearch({ scope: `project:${activeProject.id}` })}
							className={cn(
								'rounded px-2 py-1 font-medium transition-colors',
								activeScope === `project:${activeProject.id}`
									? 'bg-background text-foreground shadow-xs'
									: 'text-muted-foreground hover:text-foreground'
							)}
						>
							Project
						</button>
					)}
				</div>
			</div>

			<div className="h-4 w-px bg-border hidden sm:block" />

			{/* ── System multi-select ── */}
			<div className="flex items-center gap-1.5" role="group" aria-label="Systems">
				<span className="font-semibold text-muted-foreground uppercase text-[10px] tracking-wider mr-1">
					Engine
				</span>
				<div className="inline-flex items-center gap-1">
					{SYSTEMS.map((sys) => {
						const isSelected = activeSystems.includes(sys.id);
						return (
							<button
								key={sys.id}
								type="button"
								data-system={sys.id}
								aria-pressed={isSelected}
								onClick={() => toggleSystem(sys.id)}
								className={cn(
									'flex items-center gap-1 rounded-md border px-2 py-0.5 text-[11px] font-medium transition-colors',
									isSelected
										? 'border-primary/50 bg-primary/10 text-primary'
										: 'border-border bg-background text-muted-foreground hover:text-foreground opacity-60'
								)}
							>
								{isSelected && <Check className="h-3 w-3" />}
								<span>{sys.label}</span>
							</button>
						);
					})}
					<button
						type="button"
						data-sys-all
						onClick={() => setAllSystems(true)}
						className="text-[10px] text-muted-foreground hover:text-foreground px-1"
					>
						all
					</button>
					<span className="text-[10px] text-muted-foreground">·</span>
					<button
						type="button"
						data-sys-none
						onClick={() => setAllSystems(false)}
						className="text-[10px] text-muted-foreground hover:text-foreground px-1"
					>
						none
					</button>
				</div>
			</div>

			<div className="h-4 w-px bg-border hidden sm:block" />

			{/* ── Kind filter ── */}
			<div
				className={cn('flex items-center gap-1.5', isAnalyze && 'opacity-40 pointer-events-none')}
				role="group"
				aria-label="Kind"
			>
				<span className="font-semibold text-muted-foreground uppercase text-[10px] tracking-wider mr-1">
					Kind
				</span>
				<div className="inline-flex rounded-md bg-muted p-0.5">
					{KINDS.map((k) => (
						<button
							key={k.id}
							type="button"
							data-kind={k.id}
							aria-pressed={activeKind === k.id}
							disabled={isAnalyze}
							onClick={() => updateSearch({ kind: k.id })}
							className={cn(
								'rounded px-2 py-1 font-medium transition-colors',
								activeKind === k.id
									? 'bg-background text-foreground shadow-xs'
									: 'text-muted-foreground hover:text-foreground'
							)}
						>
							{k.label}
						</button>
					))}
				</div>
			</div>
		</div>
	);
}
