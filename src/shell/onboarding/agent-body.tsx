// Step 2 â Coding agent picker.
//
// Renders a fixed list of supported engines as cards with skeleton rows
// while their PATH lookups resolve. Each engine probe fires independently
// so the slowest one never blocks the fastest from revealing. The chosen
// engineId is written through to settings_kv via `setDefaultEngineId` so
// the choice survives "Clear local data". Empty-state still offers the
// engine-noop offline CTA inherited from the previous flow.
//
// Tests rely on `OFFLINE_PAYLOAD`, `agentToPayload`, `shouldShowAuthWarning`,
// and `findEngineNoopEntry` â keep those exports stable.

import { useMutation } from '@tanstack/react-query';
import { openExternalUrl } from '@/lib/transport/shims';
import { useEffect, useState } from 'react';

import { LoreTerm } from '@/components/lore/lore-term';
import { Button } from '@/components/ui/button';
import { StatusChip } from '@/components/ui/status-chip';
import { cn } from '@/components/ui/utils';
import {
	fetchIndex,
	fetchPkgDetail,
	type RegistryEntry,
	type RegistryIndex,
	resolveInstallPlan,
} from '@/lib/registry/client';
import { useShellStore } from '@/lib/shell/shell-store';
import { type AgentDetectEntry, useAgentDetect } from '@/lib/shell/use-agent-detect';
import { type DetectedAgent, pkgInstallFromRegistry, pkgKernelStatus } from '@/lib/tauri-cmd';
import { EngineLogo } from '@/shell/onboarding/engine-logo';

import { useOnboardingStep } from './use-onboarding-step';

export interface AgentStepPayload {
	agentId: string;
	display?: string;
	executablePath?: string;
	version?: string | null;
	authed?: boolean | null;
}

interface AgentBodyProps {
	onContinue: () => void;
}

const OFFLINE_AGENT_ID = 'engine-noop';
const ENGINE_NOOP_NPM_NAME = '@ikenga/pkg-engine-noop';
const ENGINE_NOOP_PKG_ID = 'com.ikenga.engine-noop';
const REGISTRY_UNREACHABLE_MSG =
	"Couldn't reach the registry â you can install the offline engine later from Packages â Browse.";

// Stable display order. The Rust side already knows about these ids in
// `KNOWN_AGENTS`; the wizard surfaces them whether the binary is present
// or not so the user sees the full menu of supported engines.
const SUPPORTED_ENGINES: ReadonlyArray<{
	id: string;
	display: string;
	description: string;
	binaryHint: string;
	docsUrl: string;
	installCmd?: string;
}> = [
	{
		id: 'claude-code',
		display: 'Claude Code',
		description: 'Anthropic â full ACP capabilities, MCP, thinking, resume.',
		binaryHint: 'claude',
		docsUrl: 'https://docs.anthropic.com/en/docs/claude-code',
		installCmd: 'npm install -g @anthropic-ai/claude-code',
	},
	{
		id: 'codex',
		display: 'OpenAI Codex CLI',
		description: 'OpenAI â streaming + tool use. No MCP yet.',
		binaryHint: 'codex',
		docsUrl: 'https://platform.openai.com/docs/guides/codex',
		installCmd: 'npm install -g @openai/codex',
	},
	{
		id: 'gemini',
		display: 'Gemini CLI',
		description: 'Google â streaming + tool use.',
		binaryHint: 'gemini',
		docsUrl: 'https://github.com/google-gemini/gemini-cli',
		installCmd: 'npm install -g @google/gemini-cli',
	},
	{
		id: 'cursor-agent',
		display: 'Cursor Agent',
		description: 'Cursor â€” streaming, tool use, MCP.',
		binaryHint: 'cursor-agent',
		docsUrl: 'https://docs.cursor.com/en/cli',
	},
	{
		id: 'opencode',
		display: 'OpenCode',
		description: 'Open source â€” terminal coding agent, subagents, MCP, models.',
		binaryHint: 'opencode',
		docsUrl: 'https://opencode.ai',
		installCmd: 'npm install -g opencode-ai',
	},
	{
		id: 'pi',
		display: 'Pi Coding Agent',
		description: 'Minimalist coding harness â€” multi-provider, edit, bash tools.',
		binaryHint: 'pi',
		docsUrl: 'https://github.com/earendil-works/pi',
		installCmd: 'npm install -g @earendil-works/pi-coding-agent',
	},
	{
		id: 'ollama',
		display: 'Ollama',
		description: 'Local models â€” terminal use, no tool use yet.',
		binaryHint: 'ollama',
		docsUrl: 'https://ollama.com',
	},
];

const SUPPORTED_ENGINE_IDS = SUPPORTED_ENGINES.map((e) => e.id);

export function AgentBody({ onContinue }: AgentBodyProps) {
	const { record, setPayload } = useOnboardingStep<AgentStepPayload>('agent');
	const selectedAgentId = useShellStore((s) => s.onboarding.selectedAgentId);
	const setSelectedAgentId = useShellStore((s) => s.setSelectedAgentId);

	const { results, refresh } = useAgentDetect(SUPPORTED_ENGINE_IDS);
	const isOffline = selectedAgentId === OFFLINE_AGENT_ID;

	// Pre-select the first detected engine the first time the user lands
	// here. Honour any existing choice so re-entering from Settings keeps
	// the user's prior pick highlighted.
	// biome-ignore lint/correctness/useExhaustiveDependencies: `applySelection` closes over stable Zustand setters; including it would re-run on every render without changing behaviour.
	useEffect(() => {
		if (selectedAgentId) return;
		const firstDetected = SUPPORTED_ENGINE_IDS.find((id) => results[id]?.status === 'detected');
		if (!firstDetected) return;
		const agent = results[firstDetected]?.agent;
		if (!agent) return;
		applySelection(agent);
	}, [results, selectedAgentId]);

	function applySelection(agent: DetectedAgent) {
		setSelectedAgentId(agent.id);
		setPayload(agentToPayload(agent));
	}

	const handleSelect = (agent: DetectedAgent) => {
		applySelection(agent);
	};

	// âââ Manual override ââââââââââââââââââââââââââââââââââââââââââââââââââ
	const [overridePath, setOverridePath] = useState('');
	const [overrideError, setOverrideError] = useState<string | null>(null);
	const [overrideBusy, setOverrideBusy] = useState(false);

	async function handleOverrideApply() {
		const path = overridePath.trim();
		if (!path) {
			setOverrideError('Enter an absolute path to the binary.');
			return;
		}
		if (!path.startsWith('/') && !/^[A-Za-z]:\\/.test(path)) {
			setOverrideError('Use an absolute path (start with / on Unix or a drive letter on Windows).');
			return;
		}
		setOverrideBusy(true);
		setOverrideError(null);
		try {
			// Spawn-and-respond is the only verification we owe the user â the
			// engine adapter will surface a clear error if the binary fails on
			// first send. Pin a generic 'custom' id and stash the path in the
			// payload so the adapter can pick it up.
			const customAgent: DetectedAgent = {
				id: 'custom',
				display: 'Custom binary',
				executable_path: path,
				version: null,
				authed: null,
				auth_hint: null,
				capabilities: {
					streaming: true,
					tool_use: false,
					thinking: false,
					artifacts: false,
					mcp: false,
					session_resume: false,
				},
			};
			applySelection(customAgent);
		} finally {
			setOverrideBusy(false);
		}
	}

	// âââ Offline fallback (engine-noop install) âââââââââââââââââââââââââââ
	const [offlineError, setOfflineError] = useState<string | null>(null);

	const offlineMut = useMutation({
		mutationFn: async () => {
			const status = await pkgKernelStatus();
			if (status.installed.some((p) => p.id === ENGINE_NOOP_PKG_ID)) return;

			const { index, indexUrl } = await fetchIndex();
			const entry = findEngineNoopEntry(index);
			if (!entry) {
				throw new Error('registry index has no @ikenga/pkg-engine-noop entry');
			}

			const detail = await fetchPkgDetail(indexUrl, entry);
			const plan = await resolveInstallPlan(detail, (name) => fetchPkgDetail(indexUrl, { name }));

			for (const step of plan) {
				try {
					await pkgInstallFromRegistry({
						tarball: step.tarball,
						integrity: step.integrity,
						pkgId: step.pkgId,
						sourceUrl: step.tarball,
						// Signed-index publisher key (WP-06 will populate it;
						// undefined today -> installs untrusted-for-elevated).
						publisherKey: (step as { publisherKey?: string | null }).publisherKey ?? undefined,
					});
				} catch (e) {
					const msg = String((e as Error).message ?? e).toLowerCase();
					if (msg.includes('already installed') || msg.includes('already registered')) {
						continue;
					}
					throw e;
				}
			}
		},
		onSuccess: () => {
			setOfflineError(null);
			setSelectedAgentId(OFFLINE_AGENT_ID);
			setPayload(OFFLINE_PAYLOAD);
		},
		onError: (e) => {
			const raw = (e as Error).message ?? String(e);
			if (/signature|sig|verify/i.test(raw)) {
				console.error('[onboarding] engine-noop install: signature verification failed', e);
			} else if (/integrity|sha-?512/i.test(raw)) {
				console.error('[onboarding] engine-noop install: tarball integrity mismatch', e);
			} else {
				console.error('[onboarding] engine-noop install failed', e);
			}
			setOfflineError(REGISTRY_UNREACHABLE_MSG);
		},
	});

	const handleOffline = () => {
		if (offlineMut.isPending) return;
		setOfflineError(null);
		offlineMut.mutate();
	};

	const anyDetected = SUPPORTED_ENGINE_IDS.some((id) => results[id]?.status === 'detected');
	const anyPending = SUPPORTED_ENGINE_IDS.some((id) => results[id]?.status === 'pending');
	const allMissing = !anyPending && !anyDetected;

	const offlineButtonLabel = (long: boolean) => {
		if (offlineMut.isPending) return 'Installing offline engineâ¦';
		if (isOffline) return 'Offline selected';
		return long ? 'Use offline mode' : 'Continue offline';
	};

	return (
		<div className="mx-auto max-w-5xl">
			<div className="mb-8 flex items-end justify-between gap-6">
				<div>
					<p
						className="mb-2 text-xs font-semibold uppercase tracking-[0.04em]"
						style={{ color: 'var(--primary)' }}
					>
						Choose your <LoreTerm term="Chi">Chi</LoreTerm>
					</p>
					<h1 className="text-3xl font-bold leading-tight tracking-tight">
						Which <LoreTerm term="Chi">Chi</LoreTerm> should drive your{' '}
						<LoreTerm term="Ikenga">Ikenga</LoreTerm>?
					</h1>
					<p className="mt-2 max-w-[60ch] text-sm" style={{ color: 'var(--fg-muted)' }}>
						{anyPending
							? 'Scanning your $PATH for each agent in parallelâ¦'
							: anyDetected
								? 'Pick one to continue. You can change your Chi later from Settings â Engine.'
								: "We couldn't find any Chi on $PATH. Install one below, point at a custom binary, or continue offline."}
					</p>
				</div>
				<Button variant="secondary" size="sm" onClick={() => refresh()} data-testid="agents-rescan">
					Re-scan
				</Button>
			</div>

			{/* ââ Engine grid (always 5 cards; status reveals per-engine) ââââ */}
			<div className="grid gap-4 md:grid-cols-2" data-testid="agents-grid">
				{SUPPORTED_ENGINES.map((engine) => {
					const entry = results[engine.id] ?? { status: 'pending' as const };
					return (
						<EngineCard
							key={engine.id}
							meta={engine}
							entry={entry}
							selected={selectedAgentId === engine.id}
							onSelect={() => {
								if (entry.status === 'detected' && entry.agent) {
									handleSelect(entry.agent);
								}
							}}
							onOpenDocs={() => void openExternalUrl(engine.docsUrl).catch(() => {})}
						/>
					);
				})}
			</div>

			{/* ââ Custom binary override âââââââââââââââââââââââââââââââââââââ */}
			<details
				className="mt-6 rounded-md border p-4"
				style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-surface)' }}
			>
				<summary
					className="cursor-pointer text-sm font-medium"
					style={{ color: 'var(--fg-muted)' }}
					data-testid="agents-override-toggle"
				>
					Pick another binary
				</summary>
				<div className="mt-3 grid gap-3">
					<p className="text-xs" style={{ color: 'var(--fg-faint)' }}>
						Absolute path to a coding-agent CLI. Use this when your install lives outside $PATH or
						you're sandbox-testing a fork.
					</p>
					<div className="flex gap-2">
						<input
							type="text"
							value={overridePath}
							onChange={(e) => setOverridePath(e.target.value)}
							placeholder="/usr/local/bin/my-agent"
							className="flex-1 rounded-md border px-3 py-1.5 font-mono text-xs"
							style={{
								borderColor: 'var(--border-soft)',
								background: 'var(--bg-raised)',
								color: 'var(--fg)',
							}}
							data-testid="agents-override-input"
						/>
						<Button
							variant="secondary"
							size="sm"
							onClick={() => void handleOverrideApply()}
							disabled={overrideBusy}
							data-testid="agents-override-apply"
						>
							{overrideBusy ? 'Verifyingâ¦' : 'Use this binary'}
						</Button>
					</div>
					{overrideError && (
						<div
							className="rounded-md border p-2 text-xs"
							style={{
								borderColor: 'var(--danger)',
								background: 'var(--danger-soft)',
								color: 'var(--fg)',
							}}
							data-testid="agents-override-error"
						>
							{overrideError}
						</div>
					)}
				</div>
			</details>

			{/* ââ Offline-mode strip ââââââââââââââââââââââââââââââââââââââââ */}
			<div
				className="mt-6 flex items-center gap-4 rounded-md border border-dashed p-4"
				style={{ borderColor: 'var(--border-strong)' }}
			>
				<div
					className="flex h-7 w-7 flex-none items-center justify-center rounded-full text-xs font-bold"
					style={{
						background: 'var(--info, var(--bg-raised))',
						color: 'var(--info-fg, var(--fg))',
					}}
					aria-hidden="true"
				>
					i
				</div>
				<div className="flex-1">
					<div className="text-[13px] font-semibold">
						{allMissing
							? 'No Chi found on this machine â continue without one'
							: 'Continue without a Chi'}
					</div>
					<div className="mt-0.5 text-xs" style={{ color: 'var(--fg-muted)' }}>
						Pkg management, files, terminal, and project routing still work. Your Ikenga can be
						addressed once you connect one.
					</div>
				</div>
				<Button
					variant={isOffline ? 'default' : 'secondary'}
					size="sm"
					onClick={handleOffline}
					disabled={offlineMut.isPending}
					data-testid="agents-offline-cta"
				>
					{offlineButtonLabel(false)}
				</Button>
			</div>
			{offlineError && (
				<div
					role="alert"
					className="mt-3 rounded-md border p-3 text-xs"
					style={{
						borderColor: 'var(--danger)',
						background: 'var(--danger-soft)',
						color: 'var(--fg)',
					}}
					data-testid="agents-offline-error"
				>
					{offlineError}
				</div>
			)}

			{/* ââ Inline Continue ââââââââââââââââââââââââââââââââââââââââââââ */}
			<div className="mt-8 flex items-center justify-end gap-3">
				<Button
					onClick={onContinue}
					disabled={!selectedAgentId}
					data-testid="agent-inline-continue"
				>
					{selectedAgentId ? 'Continue' : 'Pick an agent or use offline mode'}
				</Button>
			</div>

			{record.payload?.agentId && record.payload.agentId !== selectedAgentId && (
				<p
					className="mt-3 text-right text-xs"
					style={{ color: 'var(--fg-faint)' }}
					data-testid="agent-record-hint"
				>
					Previously selected: {record.payload.display ?? record.payload.agentId}
				</p>
			)}
		</div>
	);
}

interface EngineCardProps {
	meta: (typeof SUPPORTED_ENGINES)[number];
	entry: AgentDetectEntry;
	selected: boolean;
	onSelect: () => void;
	onOpenDocs: () => void;
}

function EngineCard({ meta, entry, selected, onSelect, onOpenDocs }: EngineCardProps) {
	const interactive = entry.status === 'detected';
	// `div role="button"` (not a real <button>) so the "Docs â" affordance can
	// nest inside â the HTML spec and React forbid <button> inside <button>.
	const handleKey = (e: React.KeyboardEvent<HTMLDivElement>) => {
		if (!interactive) return;
		if (e.key === 'Enter' || e.key === ' ') {
			e.preventDefault();
			onSelect();
		}
	};
	return (
		<div
			role="button"
			tabIndex={interactive ? 0 : -1}
			aria-disabled={!interactive}
			aria-pressed={selected}
			onClick={interactive ? onSelect : undefined}
			onKeyDown={handleKey}
			className={cn(
				'relative rounded-lg border p-5 text-left transition-colors',
				selected
					? 'shadow-sm'
					: interactive
						? 'hover:border-[var(--border-strong)] cursor-pointer'
						: 'cursor-not-allowed'
			)}
			style={{
				borderColor: selected ? 'var(--primary)' : 'var(--border-soft)',
				background: 'var(--bg-surface)',
				boxShadow: selected ? '0 0 0 1px var(--primary)' : undefined,
				opacity: interactive ? 1 : 0.85,
			}}
			data-testid="agent-card"
			data-agent-id={meta.id}
			data-selected={selected}
			data-status={entry.status}
		>
			{selected && (
				<span
					className="absolute right-3 top-3 flex h-5 w-5 items-center justify-center rounded-full text-[11px] font-bold"
					style={{ background: 'var(--primary)', color: 'var(--primary-fg, white)' }}
					aria-hidden="true"
				>
					â
				</span>
			)}

			<div className="mb-3 flex items-center gap-3">
				<div
					className="flex h-9 w-9 flex-none items-center justify-center overflow-hidden rounded-md"
					style={{ background: 'var(--bg-raised)' }}
					aria-hidden="true"
				>
					<EngineLogo engineId={meta.id} size={28} />
				</div>
				<div className="min-w-0">
					<div className="truncate text-[15px] font-bold leading-tight">{meta.display}</div>
					<div className="mt-0.5 text-[11.5px]" style={{ color: 'var(--fg-faint)' }}>
						{meta.description}
					</div>
				</div>
			</div>

			<div className="flex flex-wrap items-center gap-1.5">
				<StatusPill entry={entry} />
				{entry.status === 'detected' && entry.agent?.version && (
					<Pill>
						<span className="font-mono text-[11px]">{entry.agent.version}</span>
					</Pill>
				)}
				{entry.status === 'detected' && <AuthPill authed={entry.agent?.authed ?? null} />}
			</div>

			<div
				className="mt-3 border-t border-dashed pt-3 text-xs"
				style={{ borderColor: 'var(--border-soft)' }}
			>
				{entry.status === 'pending' ? (
					<SkeletonRow />
				) : entry.status === 'detected' && entry.agent ? (
					<div className="flex gap-3">
						<span className="w-20 flex-none" style={{ color: 'var(--fg-faint)' }}>
							Binary
						</span>
						<span className="truncate font-mono text-[11.5px]" title={entry.agent.executable_path}>
							{entry.agent.executable_path}
						</span>
					</div>
				) : (
					<div className="flex items-center justify-between gap-2">
						<span style={{ color: 'var(--fg-faint)' }}>
							Not on $PATH. Try: <span className="font-mono text-[11.5px]">{meta.binaryHint}</span>
						</span>
						<button
							type="button"
							onClick={(e) => {
								e.stopPropagation();
								onOpenDocs();
							}}
							className="text-[11.5px] underline-offset-2 hover:underline"
							style={{ color: 'var(--primary)' }}
						>
							Docs â
						</button>
					</div>
				)}
			</div>
			{entry.status === 'missing' && meta.installCmd && (
				<div
					className="mt-2 font-mono text-[11px]"
					style={{ color: 'var(--fg-faint)' }}
					data-testid="agent-install-cmd"
				>
					{meta.installCmd}
				</div>
			)}
		</div>
	);
}

function SkeletonRow() {
	return (
		<div className="flex gap-3" aria-hidden="true" data-testid="agent-skeleton">
			<span
				className="block h-3 w-20 animate-pulse rounded"
				style={{ background: 'var(--bg-raised)' }}
			/>
			<span
				className="block h-3 flex-1 animate-pulse rounded"
				style={{ background: 'var(--bg-raised)' }}
			/>
		</div>
	);
}

function StatusPill({ entry }: { entry: AgentDetectEntry }) {
	if (entry.status === 'pending') {
		return (
			<span data-testid="status-pill" data-status="pending">
				<StatusChip tone="faint" dot className="animate-pulse">
					Scanningâ¦
				</StatusChip>
			</span>
		);
	}
	if (entry.status === 'detected') {
		return (
			<span data-testid="status-pill" data-status="detected">
				<StatusChip tone="live" dot>
					Detected
				</StatusChip>
			</span>
		);
	}
	return (
		<span data-testid="status-pill" data-status="missing">
			<StatusChip tone="faint">Not on PATH</StatusChip>
		</span>
	);
}

function Pill({ children }: { children: React.ReactNode }) {
	return (
		<span
			className="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium"
			style={{ background: 'var(--bg-raised)', color: 'var(--fg-muted)' }}
		>
			{children}
		</span>
	);
}

function AuthPill({ authed }: { authed: boolean | null }) {
	if (authed === true) {
		return <StatusChip tone="live">signed in</StatusChip>;
	}
	if (authed === false) {
		return <StatusChip tone="warn">auth required</StatusChip>;
	}
	return null;
}

// ââ Pure helpers (testable without DOM) âââââââââââââââââââââââââââââââââ

/** Decide whether the auth-warning banner should render for the given
 *  selected agent. */
export function shouldShowAuthWarning(agent: DetectedAgent | null | undefined): boolean {
	return !!agent && agent.authed === false;
}

/** Build the payload we persist when an agent card is selected. */
export function agentToPayload(agent: DetectedAgent): AgentStepPayload {
	return {
		agentId: agent.id,
		display: agent.display,
		executablePath: agent.executable_path,
		version: agent.version,
		authed: agent.authed,
	};
}

/** The offline fallback payload â exported so the summary step and tests
 *  share the same constant. */
export const OFFLINE_PAYLOAD: AgentStepPayload = Object.freeze({
	agentId: OFFLINE_AGENT_ID,
	display: 'Offline (no engine)',
	authed: null,
});

/** Look up the engine-noop entry in a verified registry index. */
export function findEngineNoopEntry(index: RegistryIndex): RegistryEntry | undefined {
	return index.pkgs.find((p) => p.name === ENGINE_NOOP_NPM_NAME);
}
