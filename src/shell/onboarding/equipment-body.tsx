// Step 4 (D-04 `equipment`) — gather your Ngwa. Merges three shipped steps
// per WP-38's re-map (`designs/onboarding.html`'s "OLD → NEW" header
// comment and the WP-38 PR body's write-map table):
//   4 packages    (pkg-catalog grid, required connectors)         → here, "Suggested packages"
//   5 connectors  (Supabase / Resend / Listmonk forms)             → here, "Needs setup" disclosure
//   6 scaffolding (.claude/ inventory, Replace/Merge/Skip)         → here, "Found on this machine"
//                                                                     + the three-way segment
// One step, one Continue: package selection is persisted live (same as the
// shipped `packages` step); the connector forms and the `.claude/` choice
// are both resolved by the same Continue action, which (a) fires the pkg
// install batch for what's selected/not-skipped and (b) runs the scaffold
// action if the user picked anything other than "leave alone". Nothing that
// used to require its own Continue click still does — that's the point of
// folding three steps into one "equipment" gathering step.
//
// `equipment` is optional (D-04's footer shows Skip on every step but
// welcome/done) — Continue only *blocks* on a required connector being
// neither configured nor explicitly skipped, mirroring the shipped
// `connectors` step's `allHandled` gate.

import { useQuery } from '@tanstack/react-query';
import { useEffect, useMemo, useRef, useState } from 'react';

import { LoreTerm } from '@/components/lore/lore-term';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { type ChipTone, StatusChip } from '@/components/ui/status-chip';
import { cn } from '@/components/ui/utils';
import { getProvider } from '@/lib/onboarding/agent-config-providers';
import {
	CONNECTOR_REGISTRY,
	type ConnectorDef,
	type ConnectorId,
	type ConnectorStatus,
	type ConnectorTestResult,
	findConnector,
} from '@/lib/onboarding/connectors';
import {
	type PkgInstallResult,
	prewarmCatalog,
	summariseBatch,
	triggerPkgInstalls,
} from '@/lib/onboarding/install-queue';
import {
	BUCKET_LABEL,
	type CatalogIconKey,
	type CatalogTrafficLight,
	countByBucket,
	defaultSelectedIds,
	ONBOARDING_PKG_CATALOG,
	type OnboardingPkgEntry,
} from '@/lib/onboarding/pkg-catalog';
import {
	type ConnectorRequirement,
	resolveRequiredConnectors,
} from '@/lib/onboarding/resolve-connectors';
import { loadHome } from '@/lib/home';
import { useShellStore } from '@/lib/shell/shell-store';
import {
	type AgentConfigInventory,
	type ScaffoldAgentConfigMode,
	detectAgentConfig,
	scaffoldAgentConfig,
} from '@/lib/tauri-cmd';
import { WritesNote } from '@/shell/onboarding/footer';
import {
	effectiveOnboardingScope,
	onboardingClaudeRoot,
	useOnboardingScope,
} from '@/shell/onboarding/scope';
import { SettingsScopeSwitch } from '@/shell/settings/scope-switch';

import { useOnboardingStep } from './use-onboarding-step';

// D-04's three-way segment for an existing `.claude/` is `adopt` / `merge` /
// `leave` (`designs/onboarding.html` `claudeDirNote` copy) — a genuine
// rename from the shipped step's `merge` / `skip` / `scaffold` (Replace)
// vocabulary, not just a label change: "adopt" reads the existing dir as-is
// (no scaffold call, same outcome as the old default), "leave" is a NEW
// promise ("the shell will not read this project's .claude/ at all") that
// has no backing setting yet — see the WP-38 PR body's gap list. `scaffold`
// only fires on a fresh root (no existing `.claude/`); `na` is the
// non-Claude-Code / no-root cases.
export type ScaffoldingChoice = 'scaffold' | 'merge' | 'adopt' | 'leave' | 'na';

export interface EquipmentScaffoldingResult {
	choice: ScaffoldingChoice;
	rootPath: string | null;
	profile: 'starter' | 'minimal' | 'none';
	at: number;
}

export interface EquipmentStepPayload {
	/** Pkg ids the user has chosen to install (was the `packages` step). */
	selected: string[];
	/** Connectors explicitly configured / skipped (was the `connectors` step). */
	connectorsConfigured: ConnectorId[];
	connectorsSkipped: ConnectorId[];
	/** Outcome of the post-step pkg install batch — surfaced by `done`. */
	installResults?: PkgInstallResult[];
	/** The `.claude/` choice (was the `scaffolding` step). Absent until
	 *  Continue runs, or when the selected engine isn't Claude Code. */
	scaffolding?: EquipmentScaffoldingResult;
}

interface EquipmentBodyProps {
	onContinue: () => void;
	stateOverride?: 'offline';
}

type Filter = CatalogTrafficLight | 'all';
const FILTER_ORDER: readonly Filter[] = ['all', 'local-only', 'needs-cloud'];

function isEnginePkg(id: string): boolean {
	return id.startsWith('com.ikenga.engine-');
}

const CORE_PKG_IDS: ReadonlySet<string> = new Set(['com.ikenga.files']);
function isCorePkg(id: string): boolean {
	return CORE_PKG_IDS.has(id);
}

const ICON_GLYPH: Record<CatalogIconKey, string> = {
	studio: '▶',
	tasks: '◧',
	mail: '✉',
	outbound: '↗',
	content: '✎',
	sales: '$',
	files: '⎘',
	engine: '⌘',
};

// @ikenga/tokens role pairs (each `*-soft` is redefined per theme × mode in
// tokens.css), so the badges follow dark/light instead of the light-only HSL
// literals the shipped packages step carried.
const ICON_BG: Record<CatalogIconKey, { bg: string; fg: string }> = {
	studio: { bg: 'var(--agent-soft)', fg: 'var(--agent)' },
	tasks: { bg: 'var(--info-soft)', fg: 'var(--info)' },
	mail: { bg: 'var(--achievement-soft, var(--bg-raised))', fg: 'var(--achievement)' },
	outbound: { bg: 'var(--live-soft)', fg: 'var(--live)' },
	content: { bg: 'var(--systemic-soft, var(--bg-raised))', fg: 'var(--systemic)' },
	sales: { bg: 'var(--bg-raised)', fg: 'var(--fg-muted)' },
	files: { bg: 'var(--bg-raised)', fg: 'var(--fg)' },
	engine: { bg: 'color-mix(in srgb, var(--primary) 14%, transparent)', fg: 'var(--primary)' },
};

const STARTER_PREVIEW = {
	profile: 'starter' as const,
	commands: 3,
	skills: 6,
	agents: 3,
	hooks: 0,
	title: 'Music label starter',
	description:
		'Release coordination, outbound writing, and content curation. Generalised templates — no organisation-specific references.',
};

export function EquipmentBody({ onContinue, stateOverride }: EquipmentBodyProps) {
	const isOffline = stateOverride === 'offline';
	const { record, setPayload } = useOnboardingStep<EquipmentStepPayload>('equipment');
	const persisted = record.payload;

	// ── Packages ───────────────────────────────────────────────────────
	const [selected, setSelected] = useState<Set<string>>(() => {
		const seed = new Set(
			(persisted?.selected ?? defaultSelectedIds()).filter((id) => !isEnginePkg(id))
		);
		for (const id of CORE_PKG_IDS) seed.add(id);
		return seed;
	});
	const [filter, setFilter] = useState<Filter>('all');
	const [connectorsConfigured, setConnectorsConfigured] = useState<ConnectorId[]>(
		persisted?.connectorsConfigured ?? []
	);
	const [connectorsSkipped, setConnectorsSkipped] = useState<ConnectorId[]>(
		persisted?.connectorsSkipped ?? []
	);

	const seededRef = useRef(false);
	useEffect(() => {
		if (seededRef.current) return;
		seededRef.current = true;
		if (!persisted) {
			setPayload({
				selected: Array.from(selected).sort(),
				connectorsConfigured,
				connectorsSkipped,
			});
		}
	}, [persisted, selected, connectorsConfigured, connectorsSkipped, setPayload]);

	const buckets = useMemo(() => {
		const all = countByBucket();
		return { ...all, all: all.all - all.engine };
	}, []);

	const requirements = useMemo(
		() => resolveRequiredConnectors(selected, ONBOARDING_PKG_CATALOG.map((p) => p.manifest)),
		[selected]
	);

	const sizeTotal = useMemo(() => {
		let total = 0;
		for (const entry of ONBOARDING_PKG_CATALOG) {
			if (selected.has(entry.manifest.id) && entry.sizeMb) total += entry.sizeMb;
		}
		return total;
	}, [selected]);

	const visiblePkgs = useMemo(() => {
		const visible = ONBOARDING_PKG_CATALOG.filter((p) => !isEnginePkg(p.manifest.id));
		if (filter === 'all') return visible;
		return visible.filter((p) => p.bucket === filter);
	}, [filter]);

	const persistPackages = (nextSelected: Set<string>) => {
		setPayload({
			selected: Array.from(nextSelected).sort(),
			connectorsConfigured,
			connectorsSkipped,
			installResults: persisted?.installResults,
			scaffolding: persisted?.scaffolding,
		});
	};

	const toggle = (id: string) => {
		if (isEnginePkg(id) || isCorePkg(id) || isOffline) return;
		setSelected((prev) => {
			const next = new Set(prev);
			if (next.has(id)) next.delete(id);
			else next.add(id);
			persistPackages(next);
			return next;
		});
	};

	// ── Connectors (D-04 "Needs setup" disclosure per package) ───────────
	const CONNECTOR_STATUS_KEY = ['onboarding', 'connector-status'] as const;
	const statuses = useQuery({
		queryKey: [...CONNECTOR_STATUS_KEY, requirements.map((r) => r.connectorId).join(',')],
		queryFn: async () => {
			const out: Partial<Record<ConnectorId, ConnectorStatus>> = {};
			for (const req of requirements) {
				const def = findConnector(req.connectorId);
				if (!def) continue;
				try {
					out[req.connectorId] = await def.status();
				} catch {
					out[req.connectorId] = 'not_configured';
				}
			}
			return out;
		},
		staleTime: 0,
		refetchOnWindowFocus: false,
		enabled: !isOffline,
	});

	const configuredSet = useMemo(() => new Set(connectorsConfigured), [connectorsConfigured]);
	const skippedSet = useMemo(() => new Set(connectorsSkipped), [connectorsSkipped]);

	// Offline (registry unreachable) never blocks Continue — "the rest
	// continues" per `designs/onboarding.html`'s `offline` state. A connector
	// form disables its own Save/Skip while offline (network-dependent), so
	// the step-level gate has to stop requiring a decision it can't act on.
	const allConnectorsHandled =
		isOffline ||
		requirements.every(
			(r) =>
				skippedSet.has(r.connectorId) ||
				configuredSet.has(r.connectorId) ||
				(statuses.data?.[r.connectorId] ?? 'not_configured') === 'configured'
		);

	const persistConnectors = (nextConfigured: ConnectorId[], nextSkipped: ConnectorId[]) => {
		setConnectorsConfigured(nextConfigured);
		setConnectorsSkipped(nextSkipped);
		setPayload({
			selected: Array.from(selected).sort(),
			connectorsConfigured: nextConfigured,
			connectorsSkipped: nextSkipped,
			installResults: persisted?.installResults,
			scaffolding: persisted?.scaffolding,
		});
	};

	const handleConnectorConfigured = (id: ConnectorId) => {
		persistConnectors(
			Array.from(new Set([...connectorsConfigured, id])).sort(),
			connectorsSkipped.filter((x) => x !== id)
		);
		void statuses.refetch();
	};
	const handleConnectorSkip = (id: ConnectorId) => {
		persistConnectors(
			connectorsConfigured.filter((x) => x !== id),
			Array.from(new Set([...connectorsSkipped, id])).sort()
		);
	};

	// ── `.claude/` scaffolding (D-04 "Found on this machine") ────────────
	const selectedAgentId = useShellStore((s) => s.onboarding.selectedAgentId);
	const activeProject = useShellStore((s) => s.activeProject);
	const primaryRoot = activeProject?.root_path ?? activeProject?.extra_roots[0] ?? null;

	// D-04 Personal / Project scope switch (shared with the `project` step).
	// Here it picks WHICH `.claude/` the adopt / merge / scaffold choice acts
	// on: the project's own (committed with the repo) or the personal
	// `~/.claude/` (this machine only). Untouched, it stays on the project
	// whenever there is one — the shipped `scaffolding` step's only target.
	const explicitScope = useOnboardingScope((s) => s.explicit);
	const setScope = useOnboardingScope((s) => s.setScope);
	const scope = effectiveOnboardingScope(explicitScope, primaryRoot);
	const { data: homeDir } = useQuery({
		queryKey: ['onboarding', 'home-dir'],
		queryFn: loadHome,
		staleTime: Number.POSITIVE_INFINITY,
	});
	// Never touch the home `~/.claude/` implicitly: with no project root the
	// switch falls back to personal on its own, and that fallback must not
	// scaffold or merge into a directory the user never pointed at. Personal
	// is a target only once the user picks it (`explicitScope`); until then
	// the `.claude/` option is off, with the reason shown (§1.2).
	const personalChosen = scope === 'personal' && explicitScope === 'personal';
	const claudeRoot = onboardingClaudeRoot(explicitScope, primaryRoot, homeDir);
	const claudeDirLabel = !claudeRoot
		? null
		: scope === 'project'
			? '<project>/.claude/'
			: '~/.claude/';
	const isClaudeAgent = selectedAgentId === 'claude-code';

	const { data: inventory } = useQuery<AgentConfigInventory>({
		enabled: isClaudeAgent && !!claudeRoot,
		queryKey: ['onboarding', 'agent-config', 'claude-code', claudeRoot],
		queryFn: () => detectAgentConfig('claude-code', claudeRoot as string),
		refetchOnWindowFocus: false,
	});
	const hasExisting = inventory?.config_dir_present === true;

	// Only meaningful when `hasExisting` — the three-way segment doesn't
	// render on a fresh root (see the JSX below), so this default never
	// drives behaviour there.
	const [claudeDirChoice, setClaudeDirChoice] = useState<'adopt' | 'merge' | 'leave'>('adopt');

	const [busy, setBusy] = useState(false);
	const [errorMsg, setErrorMsg] = useState<string | null>(null);

	const handleContinue = async () => {
		setBusy(true);
		setErrorMsg(null);
		try {
			// (a) scaffold, if applicable.
			let scaffoldingResult: EquipmentScaffoldingResult | undefined;
			if (!isClaudeAgent || !claudeRoot) {
				scaffoldingResult = { choice: 'na', rootPath: claudeRoot, profile: 'none', at: Date.now() };
			} else if (hasExisting && claudeDirChoice === 'adopt') {
				// "Use what is there. Nothing is copied, merged or overwritten —
				// the shell just reads it." No Rust call.
				scaffoldingResult = { choice: 'adopt', rootPath: claudeRoot, profile: 'none', at: Date.now() };
			} else if (hasExisting && claudeDirChoice === 'leave') {
				// "The shell will not read this project's .claude/ at all." No
				// Rust call — and no backing setting exists yet to actually
				// suppress discovery (flagged in the WP-38 PR body); recorded
				// as intent only, same as `adopt` does today.
				scaffoldingResult = { choice: 'leave', rootPath: claudeRoot, profile: 'none', at: Date.now() };
			} else {
				// `merge` on an existing dir, or the only path on a fresh one —
				// both are additive (APPROVAL.md `augment`), never `replace`;
				// D-04 dropped the shipped step's destructive "Replace (back up
				// first)" option entirely.
				const provider = getProvider(selectedAgentId);
				const mode: ScaffoldAgentConfigMode = 'augment';
				if (provider) {
					const result = await scaffoldAgentConfig(
						provider.agentId,
						claudeRoot,
						STARTER_PREVIEW.profile,
						mode
					);
					if (!result.ok && result.errors.length > 0) {
						setErrorMsg(`${result.errors[0]?.path}: ${result.errors[0]?.reason}`);
						setBusy(false);
						return;
					}
				}
				scaffoldingResult = {
					choice: hasExisting ? 'merge' : 'scaffold',
					rootPath: claudeRoot,
					profile: STARTER_PREVIEW.profile,
					at: Date.now(),
				};
			}

			setPayload({
				selected: Array.from(selected).sort(),
				connectorsConfigured,
				connectorsSkipped,
				installResults: persisted?.installResults,
				scaffolding: scaffoldingResult,
			});

			// (b) fire package installs in the background — don't block Continue.
			if (!isOffline) {
				const skippedPkgIds = new Set<string>();
				for (const req of requirements) {
					if (skippedSet.has(req.connectorId)) {
						for (const id of req.requiredBy) skippedPkgIds.add(id);
					}
				}
				void (async () => {
					try {
						await prewarmCatalog();
						const installResults = await triggerPkgInstalls({
							selectedPkgIds: Array.from(selected),
							skippedConnectorPkgIds: skippedPkgIds,
						});
						console.info('[onboarding] equipment install batch', summariseBatch(installResults));
						setPayload({
							selected: Array.from(selected).sort(),
							connectorsConfigured,
							connectorsSkipped,
							installResults,
							scaffolding: scaffoldingResult,
						});
					} catch (e) {
						console.warn('[onboarding] equipment install batch failed', e);
					}
				})();
			}

			setBusy(false);
			onContinue();
		} catch (e) {
			setErrorMsg(String((e as Error)?.message ?? e));
			setBusy(false);
		}
	};

	return (
		<div className="mx-auto max-w-6xl" data-testid="equipment-body">
			{isOffline && (
				<div
					className="mb-6 rounded-md border p-4 text-sm"
					style={{ borderColor: 'var(--warning, var(--border-strong))', background: 'var(--warning-soft, var(--bg-surface))' }}
					data-testid="equipment-offline-notice"
				>
					<b>The registry is unreachable</b> — suggested packages are disabled. What's already on
					this machine is unaffected (read from disk, not fetched). Install suggestions later from{' '}
					<LoreTerm term="Ngwa">Ngwa</LoreTerm> → Store.
				</div>
			)}

			<div className="mb-6 flex items-end justify-between gap-6">
				<div>
					<p
						className="mb-2 text-xs font-semibold uppercase tracking-[0.04em]"
						style={{ color: 'var(--primary)' }}
					>
						<LoreTerm term="Ngwa">Ngwa</LoreTerm> — your equipment
					</p>
					<h1 className="font-display text-3xl font-bold leading-tight tracking-tight">
						Gather your <LoreTerm term="Ngwa">Ngwa</LoreTerm>.
					</h1>
					<p className="mt-2 max-w-[60ch] text-sm" style={{ color: 'var(--fg-muted)' }}>
						One catalogue of everything the Ikenga wields: skills, agents, commands, hooks, tools
						and apps. Add a few to start — change this any time from{' '}
						<span className="font-mono text-xs">Ngwa</span>.
					</p>
				</div>
				<SettingsScopeSwitch
					scope={scope}
					onScopeChange={setScope}
					projectAvailable={!!primaryRoot}
					ariaLabel="Scope"
					className="flex-none"
				/>
			</div>

			<div className="grid gap-10 lg:grid-cols-[1fr_1.2fr]">
				{/* ── Col A: found on disk + .claude/ choice ─────────────── */}
				<div>
					{isClaudeAgent && claudeRoot && hasExisting ? (
						<div
							className="rounded-lg border p-5"
							style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-surface)' }}
							data-testid="equipment-claude-dir"
						>
							<div className="text-[13px] font-semibold">
								{scope === 'project'
									? 'This project already has a .claude/ directory.'
									: 'Your personal ~/.claude/ directory already exists.'}
							</div>
							{inventory && (
								<div className="mt-3 grid grid-cols-2 gap-y-1 text-xs sm:grid-cols-4">
									<InventoryCell label="agents" value={inventory.agent_count} />
									<InventoryCell label="skills" value={inventory.skill_count} />
									<InventoryCell label="commands" value={inventory.command_count} />
									<InventoryCell label="mcp servers" value={inventory.mcp_server_count} />
								</div>
							)}
							<div
								className="mt-4 inline-flex w-fit items-center gap-1 rounded-md border p-1"
								style={{ borderColor: 'var(--border-soft)' }}
								role="group"
								aria-label="What to do with the existing .claude/"
								data-testid="equipment-claudedir-seg"
							>
								{(
									[
										['adopt', 'Adopt as-is'],
										['merge', 'Merge a starter pack'],
										['leave', 'Leave alone'],
									] as const
								).map(([id, label]) => (
									<button
										key={id}
										type="button"
										onClick={() => setClaudeDirChoice(id)}
										aria-pressed={claudeDirChoice === id}
										data-testid={`equipment-claudedir-${id}`}
										className={cn('h-7 rounded-sm px-3 text-xs font-medium')}
										style={{
											background: claudeDirChoice === id ? 'var(--bg-base)' : 'transparent',
											fontWeight: claudeDirChoice === id ? 600 : 500,
										}}
									>
										{label}
									</button>
								))}
							</div>
							<p className="mt-2 text-[11px]" style={{ color: 'var(--fg-faint)' }}>
								{claudeDirChoice === 'adopt' &&
									'Use what is there. Nothing is copied, merged or overwritten — the shell just reads it.'}
								{claudeDirChoice === 'merge' &&
									'Adds any starter skill, agent or hook that is missing. Existing files are never replaced; conflicts are listed for you to resolve.'}
								{claudeDirChoice === 'leave' &&
									`The shell will not read ${scope === 'project' ? 'this project’s .claude/' : '~/.claude/'} at all. You can turn it on later in Settings › Workspace.`}
							</p>
						</div>
					) : isClaudeAgent && claudeRoot ? (
						<div
							className="rounded-lg border p-5"
							style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-surface)' }}
							data-testid="equipment-claude-preview"
						>
							<div className="text-[13px] font-semibold">
								No .claude/ directory yet — a starter kit is ready.
							</div>
							<div className="mt-2 text-xs" style={{ color: 'var(--fg-muted)' }}>
								{STARTER_PREVIEW.description}
							</div>
							<div className="mt-3 font-mono text-xs" style={{ color: 'var(--fg-faint)' }}>
								{STARTER_PREVIEW.commands} commands · {STARTER_PREVIEW.skills} skills ·{' '}
								{STARTER_PREVIEW.agents} agents
							</div>
							<p className="mt-3 text-[11px]" style={{ color: 'var(--fg-faint)' }}>
								Writes a starter set of commands, skills and agents into .claude/ when you
								Continue. Skip this step from the footer to leave it out entirely.
							</p>
						</div>
					) : (
						<div
							className="rounded-md border border-dashed p-4 text-xs"
							style={{ borderColor: 'var(--border-soft)', color: 'var(--fg-muted)' }}
							data-testid="equipment-no-claude"
						>
							{isClaudeAgent && personalChosen ? (
								'Locating your personal ~/.claude/…'
							) : isClaudeAgent ? (
								<>
									<div data-testid="equipment-no-claude-reason">
										No project root — nothing will be written to .claude/. Add a project from the
										previous step, or choose your personal ~/.claude/ on purpose.
									</div>
									<Button
										type="button"
										size="sm"
										variant="outline"
										className="mt-3"
										onClick={() => setScope('personal')}
										data-testid="equipment-use-personal-claude"
									>
										Use my personal ~/.claude/
									</Button>
								</>
							) : (
								'.claude/ scaffolding only ships for Claude Code today. Skipped for your engine.'
							)}
						</div>
					)}

					{errorMsg && (
						<div
							className="mt-4 rounded-md border p-3 text-xs"
							style={{ borderColor: 'var(--warning, var(--border-strong))', background: 'var(--warning-soft, var(--bg-surface))' }}
							data-testid="equipment-error"
						>
							{errorMsg}
						</div>
					)}

					<WritesNote
						stepId="equipment"
						file={claudeDirLabel ? `~/.ikenga/pkgs/ + ${claudeDirLabel}` : '~/.ikenga/pkgs/'}
					/>
				</div>

				{/* ── Col B: suggested packages + connectors ─────────────── */}
				<div>
					<div
						className="mb-4 inline-flex w-fit items-center gap-1 rounded-md border p-1"
						style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-surface)' }}
						role="tablist"
						aria-label="Filter packages"
					>
						{FILTER_ORDER.map((f) => {
							const label = f === 'all' ? 'All' : BUCKET_LABEL[f];
							const count = buckets[f];
							const on = filter === f;
							return (
								<button
									key={f}
									type="button"
									role="tab"
									aria-selected={on}
									onClick={() => setFilter(f)}
									data-testid={`packages-filter-${f}`}
									className={cn('h-7 rounded-sm px-3 text-xs font-medium transition-colors')}
									style={{
										background: on ? 'var(--bg-base)' : 'transparent',
										color: on ? 'var(--fg)' : 'var(--fg-muted)',
										fontWeight: on ? 600 : 500,
									}}
								>
									{label} · {count}
								</button>
							);
						})}
					</div>

					<div className="grid gap-3 sm:grid-cols-2" data-testid="packages-grid">
						{visiblePkgs.map((entry) => (
							<PkgCard
								key={entry.manifest.id}
								entry={entry}
								selected={selected.has(entry.manifest.id)}
								isCore={isCorePkg(entry.manifest.id)}
								disabled={isOffline}
								onToggle={() => toggle(entry.manifest.id)}
							/>
						))}
					</div>
					<div className="mt-2 text-right font-mono text-xs" style={{ color: 'var(--fg-faint)' }}>
						{Array.from(selected).filter((id) => !isEnginePkg(id)).length} selected
						{sizeTotal > 0 ? ` · ≈${sizeTotal.toFixed(1)} MB` : ''}
					</div>

					{requirements.length > 0 && (
						<div className="mt-6" data-testid="equipment-connectors">
							<p
								className="mb-2 text-[11.5px] font-semibold uppercase tracking-[0.04em]"
								style={{ color: 'var(--fg-faint)' }}
							>
								Needs setup
							</p>
							<div className="space-y-4">
								{requirements.map((req) => {
									const def = findConnector(req.connectorId);
									if (!def) return null;
									return (
										<ConnectorDisclosure
											key={req.connectorId}
											connector={def}
											requirement={req}
											liveStatus={statuses.data?.[req.connectorId]}
											isSkipped={skippedSet.has(req.connectorId)}
											isConfigured={configuredSet.has(req.connectorId)}
											disabled={isOffline}
											onConfigured={() => handleConnectorConfigured(req.connectorId)}
											onSkip={() => handleConnectorSkip(req.connectorId)}
										/>
									);
								})}
							</div>
						</div>
					)}
				</div>
			</div>

			<div className="mt-8 flex items-center justify-end gap-3">
				<Button
					onClick={() => void handleContinue()}
					disabled={busy || !allConnectorsHandled}
					data-testid="equipment-inline-continue"
				>
					{busy ? 'Working…' : allConnectorsHandled ? 'Continue' : 'Configure or skip each connector'}
				</Button>
			</div>
		</div>
	);
}

function InventoryCell({ label, value }: { label: string; value: number }) {
	return (
		<div>
			<span className="font-semibold text-foreground">{value}</span>{' '}
			<span style={{ color: 'var(--fg-muted)' }}>{label}</span>
		</div>
	);
}

interface PkgCardProps {
	entry: OnboardingPkgEntry;
	selected: boolean;
	isCore: boolean;
	disabled: boolean;
	onToggle: () => void;
}

function PkgCard({ entry, selected, isCore, disabled, onToggle }: PkgCardProps) {
	const colors = ICON_BG[entry.icon];
	return (
		<button
			type="button"
			onClick={onToggle}
			disabled={isCore || disabled}
			data-testid="pkg-card"
			data-pkg-id={entry.manifest.id}
			data-selected={selected}
			data-core={isCore || undefined}
			aria-disabled={isCore || disabled}
			className={cn(
				'relative flex min-h-[150px] flex-col gap-2 rounded-lg border p-4 text-left transition-colors',
				selected ? 'shadow-sm' : 'hover:border-[var(--border-strong)]',
				(isCore || disabled) && 'cursor-default opacity-90'
			)}
			style={{
				borderColor: selected ? 'var(--primary)' : 'var(--border-soft)',
				background: 'var(--bg-surface)',
				boxShadow: selected ? '0 0 0 1px var(--primary)' : undefined,
			}}
		>
			{selected && (
				<span
					className="absolute right-2.5 top-2.5 flex h-[18px] w-[18px] items-center justify-center rounded-full text-[11px] font-bold"
					style={{ background: 'var(--primary)', color: 'var(--primary-fg, white)' }}
					aria-hidden="true"
					title={isCore ? 'Core pkg — always installed' : undefined}
				>
					{isCore ? '★' : '✓'}
				</span>
			)}
			<div className="flex items-start gap-3">
				<div
					className="flex h-8 w-8 flex-none items-center justify-center rounded-sm font-mono text-sm"
					style={{ background: colors.bg, color: colors.fg }}
					aria-hidden="true"
				>
					{ICON_GLYPH[entry.icon]}
				</div>
				<div className="min-w-0">
					<div className="text-[13.5px] font-bold leading-tight">{entry.display}</div>
					<div className="mt-0.5 font-mono text-[11px]" style={{ color: 'var(--fg-faint)' }}>
						{entry.manifest.id}
					</div>
				</div>
			</div>
			<div className="flex-1 text-[12px] leading-relaxed" style={{ color: 'var(--fg-muted)' }}>
				{entry.summary}
			</div>
		</button>
	);
}

interface ConnectorDisclosureProps {
	connector: ConnectorDef;
	requirement: ConnectorRequirement;
	liveStatus: ConnectorStatus | undefined;
	isSkipped: boolean;
	isConfigured: boolean;
	disabled: boolean;
	onConfigured: () => void;
	onSkip: () => void;
}

function ConnectorDisclosure({
	connector,
	requirement,
	liveStatus,
	isSkipped,
	isConfigured,
	disabled,
	onConfigured,
	onSkip,
}: ConnectorDisclosureProps) {
	const [values, setValues] = useState<Record<string, string>>({});
	const [saving, setSaving] = useState(false);
	const [testing, setTesting] = useState(false);
	const [saveError, setSaveError] = useState<string | null>(null);
	const [testResult, setTestResult] = useState<ConnectorTestResult | null>(null);
	const busy = saving || testing || disabled;

	const status: ConnectorStatus | 'skipped' = isSkipped
		? 'skipped'
		: isConfigured
			? 'configured'
			: (liveStatus ?? 'not_configured');

	const missing = connector.fields
		.filter((f) => f.required)
		.filter((f) => !(values[f.id] ?? '').trim());

	const handleSave = async () => {
		if (saving) return;
		setSaving(true);
		setSaveError(null);
		try {
			await connector.write(values);
			onConfigured();
		} catch (e) {
			setSaveError((e as Error).message ?? 'Save failed.');
		} finally {
			setSaving(false);
		}
	};

	const handleTest = async () => {
		if (!connector.test || testing) return;
		setTesting(true);
		try {
			setTestResult(await connector.test(values));
		} catch (e) {
			setTestResult({ ok: false, message: (e as Error).message ?? 'Test failed.' });
		} finally {
			setTesting(false);
		}
	};

	const requiredByNames = [...requirement.requiredBy].sort().join(', ');

	return (
		<div
			className="rounded-md border p-4"
			style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-surface)' }}
			data-testid={`connector-section-${connector.id}`}
			data-status={status}
		>
			<div className="mb-3 flex items-start justify-between gap-3">
				<div>
					<div className="text-[13px] font-semibold">Needs setup · {connector.display}</div>
					<div className="mt-0.5 text-[11px]" style={{ color: 'var(--fg-faint)' }}>
						Needed by: {requiredByNames}
					</div>
				</div>
				<StatusBadge status={status} />
			</div>
			<p className="mb-3 text-xs" style={{ color: 'var(--fg-muted)' }}>
				{connector.tagline}
			</p>
			<div className="space-y-3">
				{connector.fields.map((field) => (
					<div key={field.id} className="space-y-1">
						<label
							className="block text-xs font-medium"
							style={{ color: 'var(--fg-muted)' }}
							htmlFor={`${connector.id}-${field.id}`}
						>
							{field.label}
							{!field.required && (
								<span className="ml-1.5 font-normal" style={{ color: 'var(--fg-faint)' }}>
									— optional
								</span>
							)}
						</label>
						<Input
							id={`${connector.id}-${field.id}`}
							type={field.type === 'password' ? 'password' : field.type === 'url' ? 'url' : 'text'}
							value={values[field.id] ?? ''}
							onChange={(e) => setValues((prev) => ({ ...prev, [field.id]: e.target.value }))}
							placeholder={field.placeholder}
							className="h-9 font-mono text-xs"
							disabled={busy}
							data-testid={`connector-field-${connector.id}-${field.id}`}
						/>
					</div>
				))}
			</div>
			{testResult && (
				<div
					className="mt-3 rounded-md border p-2 text-xs"
					style={{ borderColor: 'var(--border-soft)' }}
					data-testid={`connector-test-result-${connector.id}`}
				>
					{testResult.ok ? 'Connection verified' : `Connection failed — ${testResult.message ?? ''}`}
				</div>
			)}
			{saveError && (
				<div
					role="alert"
					className="mt-3 rounded-md border p-2 text-xs"
					style={{ borderColor: 'var(--danger, var(--border-strong))' }}
					data-testid={`connector-save-error-${connector.id}`}
				>
					{saveError}
				</div>
			)}
			<div className="mt-3 flex flex-wrap items-center justify-end gap-2">
				{connector.test && (
					<Button
						variant="ghost"
						size="sm"
						onClick={() => void handleTest()}
						disabled={busy || missing.length > 0}
						data-testid={`connector-test-${connector.id}`}
					>
						Test connection
					</Button>
				)}
				<Button
					variant="secondary"
					size="sm"
					onClick={onSkip}
					disabled={busy}
					data-testid={`connector-skip-${connector.id}`}
				>
					Skip — install disabled
				</Button>
				<Button
					size="sm"
					onClick={() => void handleSave()}
					disabled={busy || missing.length > 0}
					data-testid={`connector-save-${connector.id}`}
				>
					{saving ? 'Saving…' : isConfigured ? 'Saved · update' : 'Save to vault'}
				</Button>
			</div>
		</div>
	);
}

function StatusBadge({ status }: { status: ConnectorStatus | 'skipped' }) {
	const toneMap: Record<ConnectorStatus | 'skipped', { tone: ChipTone; label: string }> = {
		configured: { tone: 'live', label: 'configured' },
		partial: { tone: 'warn', label: 'partial' },
		invalid: { tone: 'danger', label: 'invalid' },
		not_configured: { tone: 'faint', label: 'not configured' },
		skipped: { tone: 'faint', label: 'skipped' },
	};
	const { tone, label } = toneMap[status];
	return (
		<span data-testid="connector-status-badge">
			<StatusChip tone={tone} dot>
				{label}
			</StatusChip>
		</span>
	);
}

// ── Pure helpers exposed for tests ─────────────────────────────────────
export function previewForSelection(selectedIds: readonly string[]): ConnectorRequirement[] {
	return resolveRequiredConnectors(selectedIds, ONBOARDING_PKG_CATALOG.map((p) => p.manifest));
}

export { CONNECTOR_REGISTRY };
