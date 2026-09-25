import { useState } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { openExternalUrl } from '@/lib/transport';
import {
	AlertTriangle,
	Bot,
	CheckCircle2,
	Play,
	Plus,
	RefreshCw,
	Terminal as TerminalIcon,
	Trash2,
} from 'lucide-react';

import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import { StatusChip } from '@/components/ui/status-chip';
import { isWindows } from '@/lib/platform';
import { usePaneStore } from '@/lib/panes/pane-store';
import {
	AGENT_ENV_KEY,
	AGENT_WSL_DISTRO_KEY,
	RESUME_TERMINALS_KEY,
	useCustomShellProfiles,
	useDefaultShellProfile,
	useShellProfiles,
} from '@/lib/shell-profiles';
import { useShellStore } from '@/lib/shell/shell-store';
import {
	type DetectedAgent,
	detectAgents,
	settingsGet,
	settingsSet,
} from '@/lib/tauri-cmd';
import { writeSettingsField } from '@/lib/settings/client';
import type { SettingsAgentEnvironment, SettingsWriteOptions } from '@/lib/settings/types';
import {
	createClaudeTerminalSession,
	createTerminalSession,
} from '@/terminal/single-terminal';
import { buildAgentWrappedCmd, type AgentEngineKind } from '@/terminal/claude-wrap';
import { SettingsFieldRow, useSettingsSection } from '@/shell/settings/field';

const OFFLINE_AGENT_ID = 'engine-noop';

interface EngineFieldValueMap {
	'engines.agentEnvironment': SettingsAgentEnvironment;
	'engines.agentWslDistro': string | null;
	'engines.resumeTerminals': boolean;
}

function EnginesPage() {
	return (
		<div className="mx-auto w-full max-w-[720px] space-y-6 px-6 py-6">
			<header className="space-y-1">
				<h2
					className="text-2xl font-semibold tracking-tight"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					Chi & engines
				</h2>
				<p className="max-w-prose text-sm leading-relaxed text-muted-foreground">
					Which engine drives terminal sessions, which shells they run in, and whether terminals
					come back on start.
				</p>
			</header>

			<EngineSectionBody />
			<TerminalSectionBody />
		</div>
	);
}

function EngineSectionBody() {
	const navigate = useNavigate();
	const selectedAgentId = useShellStore((s) => s.onboarding.selectedAgentId);
	const defaultEngineId = useShellStore((s) => s.defaultEngineId);
	const payload = useShellStore(
		(s) =>
			s.onboarding.steps.agent.payload as
				| {
						agentId: string;
						display?: string;
						executablePath?: string;
						version?: string | null;
						authed?: boolean | null;
				  }
				| undefined
	);
	const enterOnboardingEdit = useShellStore((s) => s.enterOnboardingEdit);

	const {
		data: detected,
		isLoading,
		refetch,
	} = useQuery<DetectedAgent[]>({
		queryKey: ['settings', 'agent', 'detect'],
		queryFn: detectAgents,
		refetchOnWindowFocus: false,
	});

	const live = detected?.find((a) => a.id === selectedAgentId) ?? null;
	const isOffline = selectedAgentId === OFFLINE_AGENT_ID;
	const authed = live?.authed ?? payload?.authed ?? null;
	const display = live?.display ?? payload?.display ?? selectedAgentId ?? 'Not selected';
	const execPath = live?.executable_path ?? payload?.executablePath;
	const version = live?.version ?? payload?.version;

	function handleChange() {
		enterOnboardingEdit('engine');
		void navigate({ to: '/onboarding/engine' });
	}

	return (
		<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
			<header className="flex items-center justify-between border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
				<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
					Coding agent
				</h3>
				<Button variant="ghost" size="sm" onClick={() => refetch()} disabled={isLoading}>
					<RefreshCw className="mr-1 h-3 w-3" />
					Re-scan
				</Button>
			</header>
			<div className="divide-y divide-border">
				<SettingsFieldRow
					field="engines.defaultEngineId"
					label="Default engine"
					desc="Mirrors the onboarding step's choice and the default agent id."
				>
					<div className="flex items-center gap-2">
						<Bot className="h-4 w-4 text-muted-foreground" />
						<span className="text-sm font-medium text-foreground">{display}</span>
						{isOffline && (
							<span className="rounded-full bg-muted px-2 py-0.5 text-[10px] uppercase tracking-wider text-muted-foreground">
								offline
							</span>
						)}
					</div>
				</SettingsFieldRow>

				<div className="grid grid-cols-[1fr_auto] items-center gap-4 px-4 py-3">
					<div className="min-w-0 space-y-0.5">
						<div className="text-sm font-medium text-foreground">Default agent id</div>
						<div className="text-xs leading-relaxed text-muted-foreground">
							The pkg id wired as the default terminal agent. Null when offline.
						</div>
					</div>
					<span className="font-mono text-[11px] text-muted-foreground">
						{defaultEngineId ?? '(none)'}
					</span>
				</div>

				{!isOffline && (
					<>
						<div className="grid grid-cols-[1fr_auto] items-center gap-4 px-4 py-3">
							<div className="min-w-0 space-y-0.5">
								<div className="text-sm font-medium text-foreground">Auth status</div>
								<div className="text-xs leading-relaxed text-muted-foreground">
									Whether the selected agent is currently signed in / has a working API key.
								</div>
							</div>
							<AuthBadge authed={authed} loading={isLoading} />
						</div>
						<div className="grid grid-cols-[1fr_auto] items-center gap-4 px-4 py-3">
							<div className="min-w-0 space-y-0.5">
								<div className="text-sm font-medium text-foreground">Binary</div>
								<div className="text-xs leading-relaxed text-muted-foreground">
									Path the agent CLI was discovered at.
								</div>
							</div>
							<span
								className="truncate font-mono text-[11px] text-muted-foreground"
								title={execPath}
							>
								{execPath ?? '(unknown)'}
							</span>
						</div>
						{version && (
							<div className="grid grid-cols-[1fr_auto] items-center gap-4 px-4 py-3">
								<div className="min-w-0 space-y-0.5">
									<div className="text-sm font-medium text-foreground">Version</div>
									<div className="text-xs leading-relaxed text-muted-foreground">
										Reported by the agent&apos;s --version probe.
									</div>
								</div>
								<span className="font-mono text-[11px] text-muted-foreground">{version}</span>
							</div>
						)}
					</>
				)}

				<div className="flex items-center justify-between gap-4 px-4 py-3">
					<div className="min-w-0">
						<div className="text-sm font-medium text-foreground">Change agent</div>
						<div className="text-xs leading-relaxed text-muted-foreground">
							Re-runs the onboarding picker so you can rescan and pick another agent.
						</div>
					</div>
					<Button variant="default" size="sm" onClick={handleChange}>
						Change agent
					</Button>
				</div>

				{authed === false && live && (
					<div className="px-4 pb-3">
						<Banner
							tone="warning"
							icon={<AlertTriangle />}
							role="alert"
							className="rounded-md border"
							actions={
								live.auth_hint?.startsWith('http') ? (
									<button
										type="button"
										onClick={() => void openExternalUrl(live.auth_hint!).catch(() => {})}
										className="text-xs underline-offset-2 hover:underline"
										style={{ color: 'var(--primary)' }}
									>
										Open docs →
									</button>
								) : undefined
							}
						>
							<div className="text-[13px] font-semibold">{live.display} isn&apos;t signed in</div>
							<div className="mt-1 text-xs" style={{ color: 'var(--fg-muted)' }}>
								{live.auth_hint ??
									'Run the agent CLI once to authenticate, or set the relevant API key in your environment.'}
							</div>
						</Banner>
					</div>
				)}
			</div>
		</section>
	);
}

function AuthBadge({ authed, loading }: { authed: boolean | null; loading: boolean }) {
	if (loading && authed == null) {
		return <StatusChip tone="muted">Checking…</StatusChip>;
	}
	if (authed === true) {
		return (
			<StatusChip tone="live" dot>
				Signed in
			</StatusChip>
		);
	}
	if (authed === false) {
		return (
			<StatusChip tone="warn" dot>
				Auth required
			</StatusChip>
		);
	}
	return <StatusChip tone="muted">Unknown</StatusChip>;
}

function TerminalSectionBody() {
	const { scope, projectId, result, refresh } = useSettingsSection();
	const queryClient = useQueryClient();
	const { profiles, selectedProfile, setDefaultProfileId, isLoading } = useDefaultShellProfile();
	const { refetch: refetchProfiles, isFetching } = useShellProfiles();
	const { addCustomProfile, removeCustomProfile } = useCustomShellProfiles();

	const [isAddingCustom, setIsAddingCustom] = useState(false);
	const [customLabel, setCustomLabel] = useState('');
	const [customCommand, setCustomCommand] = useState('');

	const isProject = scope === 'project';
	const engines = result?.effective.engines ?? {};
	const agentEnvOverride = engines.agentEnvironment;
	const agentWslDistroOverride = engines.agentWslDistro;
	const resumeTerminalsOverride = engines.resumeTerminals;

	async function writeEngine<K extends keyof EngineFieldValueMap>(
		field: K,
		value: EngineFieldValueMap[K]
	) {
		if (!projectId) return;
		await writeSettingsField({ scope: 'project', field, value, projectId } as SettingsWriteOptions);
		refresh();
	}

	const agentEnvQuery = useQuery<string>({
		queryKey: ['settings', AGENT_ENV_KEY],
		queryFn: async () => {
			const res = await settingsGet(AGENT_ENV_KEY);
			return res ?? 'native';
		},
		enabled: !isProject,
	});

	const agentWslDistroQuery = useQuery<string | null>({
		queryKey: ['settings', AGENT_WSL_DISTRO_KEY],
		queryFn: async () => {
			return await settingsGet(AGENT_WSL_DISTRO_KEY);
		},
		enabled: !isProject,
	});

	const agentEnvMutation = useMutation({
		mutationFn: async (envKind: string) => {
			if (isProject) {
				await writeEngine('engines.agentEnvironment', envKind as SettingsAgentEnvironment);
				return;
			}
			await settingsSet(AGENT_ENV_KEY, envKind);
		},
		onSuccess: () => {
			void queryClient.invalidateQueries({ queryKey: ['settings', AGENT_ENV_KEY] });
		},
	});

	const agentDistroMutation = useMutation({
		mutationFn: async (distro: string) => {
			if (isProject) {
				await writeEngine('engines.agentWslDistro', distro);
				return;
			}
			await settingsSet(AGENT_WSL_DISTRO_KEY, distro);
		},
		onSuccess: () => {
			void queryClient.invalidateQueries({ queryKey: ['settings', AGENT_WSL_DISTRO_KEY] });
		},
	});

	const resumeTerminalsQuery = useQuery<string>({
		queryKey: ['settings', RESUME_TERMINALS_KEY],
		queryFn: async () => {
			const res = await settingsGet(RESUME_TERMINALS_KEY);
			return res ?? 'true';
		},
		enabled: !isProject,
	});

	const resumeTerminalsMutation = useMutation({
		mutationFn: async (value: boolean) => {
			if (isProject) {
				await writeEngine('engines.resumeTerminals', value);
				return;
			}
			await settingsSet(RESUME_TERMINALS_KEY, value ? 'true' : 'false');
		},
		onSuccess: () => {
			void queryClient.invalidateQueries({ queryKey: ['settings', RESUME_TERMINALS_KEY] });
		},
	});

	const wslProfiles = profiles.filter((p) => p.kind === 'wsl');
	const currentAgentEnv =
		(isProject ? agentEnvOverride : agentEnvQuery.data) ?? 'native';
	const currentAgentDistro =
		(isProject ? agentWslDistroOverride : agentWslDistroQuery.data) ??
		(wslProfiles.length > 0 ? (wslProfiles[0].distro ?? 'default') : null);
	const resumeTerminalsEffective = isProject
		? (resumeTerminalsOverride ?? true)
		: resumeTerminalsQuery.data === 'true';

	function openTestTerminal(cmd: string[], title: string) {
		const focusedId = usePaneStore.getState().focusedId;
		const sessionId = createTerminalSession({ cmd, title });
		usePaneStore.getState().addTab(focusedId, { kind: 'terminal', sessionId });
	}

	function openAgentTerminal(engine: AgentEngineKind, title: string) {
		const isWsl = currentAgentEnv === 'wsl';
		const shellTarget = isWsl ? 'wsl' : isWindows ? 'native' : 'posix';
		const wslDistro = isWsl ? currentAgentDistro : undefined;
		const focusedId = usePaneStore.getState().focusedId;

		if (engine === 'claude') {
			const sessionId = createClaudeTerminalSession({
				shellTarget,
				wslDistro: wslDistro ?? undefined,
			});
			usePaneStore.getState().addTab(focusedId, { kind: 'terminal', sessionId });
			return;
		}

		const cmd = buildAgentWrappedCmd({ engine, shellTarget, wslDistro });
		const sessionId = createTerminalSession({ cmd, title });
		usePaneStore.getState().addTab(focusedId, { kind: 'terminal', sessionId });
	}

	const handleSaveCustomProfile = () => {
		if (!customLabel.trim() || !customCommand.trim()) return;
		const parts = customCommand.trim().split(/\s+/);
		addCustomProfile({
			label: customLabel.trim(),
			icon: 'terminal',
			cmd: parts,
			kind: 'custom',
			distro: null,
		});
		setCustomLabel('');
		setCustomCommand('');
		setIsAddingCustom(false);
	};

	return (
		<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
			<header className="flex items-center justify-between border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
				<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
					Terminal & shells
				</h3>
				<Button
					variant="ghost"
					size="sm"
					className="h-7 gap-1.5 text-xs text-muted-foreground hover:text-foreground"
					onClick={() => void refetchProfiles()}
					disabled={isFetching}
				>
					<RefreshCw className={`h-3 w-3 ${isFetching ? 'animate-spin' : ''}`} />
					Rescan shells
				</Button>
			</header>
			<div className="divide-y divide-border">
				<SettingsFieldRow
					field="engines.defaultShellId"
					label="Default interactive shell"
					desc="The shell used when opening a new terminal tab via shortcuts (Ctrl+T) or the tab strip."
				>
					<select
						value={selectedProfile.id}
						onChange={(e) => setDefaultProfileId(e.target.value)}
						className="rounded-md border border-border bg-background px-3 py-1.5 text-xs font-medium text-foreground outline-none focus:ring-1 focus:ring-primary"
						disabled={isLoading}
					>
						{profiles.map((p) => (
							<option key={p.id} value={p.id}>
								{p.label} ({p.cmd.join(' ')})
							</option>
						))}
					</select>
				</SettingsFieldRow>

				{isWindows && (
					<SettingsFieldRow
						field="engines.agentEnvironment"
						label="Agent execution target"
						desc="Select whether coding agents like Claude Code CLI run inside Native Windows or inside a WSL Linux environment."
					>
						<div className="flex items-center gap-3">
							<label className="flex cursor-pointer items-center gap-1.5 text-xs">
								<input
									type="radio"
									name="agentEnv"
									value="native"
									checked={currentAgentEnv === 'native'}
									onChange={(e) => agentEnvMutation.mutate(e.target.value)}
								/>
								<span>Native (PowerShell/CMD)</span>
							</label>

							{wslProfiles.length > 0 && (
								<label className="flex cursor-pointer items-center gap-1.5 text-xs">
									<input
										type="radio"
										name="agentEnv"
										value="wsl"
										checked={currentAgentEnv === 'wsl'}
										onChange={(e) => agentEnvMutation.mutate(e.target.value)}
									/>
									<span>WSL (Linux)</span>
								</label>
							)}
						</div>
					</SettingsFieldRow>
				)}

				{isWindows && currentAgentEnv === 'wsl' && wslProfiles.length > 0 && (
					<SettingsFieldRow
						field="engines.agentWslDistro"
						label="WSL distribution"
						desc="Which installed WSL distribution to run Claude and coding tools inside."
					>
						<select
							value={currentAgentDistro ?? ''}
							onChange={(e) => agentDistroMutation.mutate(e.target.value)}
							className="rounded-md border border-border bg-background px-3 py-1.5 text-xs font-medium text-foreground outline-none focus:ring-1 focus:ring-primary"
						>
							{wslProfiles.map((p) => (
								<option key={p.id} value={p.distro ?? 'default'}>
									{p.label}
								</option>
							))}
						</select>
					</SettingsFieldRow>
				)}

				<SettingsFieldRow
					field="engines.resumeTerminals"
					label="Resume terminals on start"
					desc="Respawn previously running terminals when Ikenga starts, including tabs in unfocused panes."
				>
					<label className="flex items-center gap-2 text-sm">
						<input
							type="checkbox"
							className="h-4 w-4 rounded border-border bg-background"
							checked={resumeTerminalsEffective}
							onChange={(e) => resumeTerminalsMutation.mutate(e.target.checked)}
							disabled={resumeTerminalsQuery.isLoading && !isProject}
						/>
						<span className={!resumeTerminalsEffective ? 'text-muted-foreground' : ''}>Enabled</span>
					</label>
				</SettingsFieldRow>

				<div className="flex items-center justify-between px-4 py-3">
					<div className="space-y-0.5">
						<div className="flex items-center gap-2 text-sm font-medium text-foreground">
							<Bot className="h-4 w-4 text-primary" /> Claude Code CLI
						</div>
						<div className="text-xs text-muted-foreground">
							Interactive Claude session with crash protection and exit codes.
						</div>
					</div>
					<Button
						variant="outline"
						size="sm"
						className="h-7 gap-1.5 text-xs"
						onClick={() => openAgentTerminal('claude', 'claude')}
					>
						<Play className="h-3 w-3" /> Launch Claude
					</Button>
				</div>

				<div className="flex items-center justify-between px-4 py-3">
					<div className="space-y-0.5">
						<div className="flex items-center gap-2 text-sm font-medium text-foreground">
							<Bot className="h-4 w-4 text-primary" /> Antigravity CLI (agy)
						</div>
						<div className="text-xs text-muted-foreground">
							Interactive Antigravity assistant terminal.
						</div>
					</div>
					<Button
						variant="outline"
						size="sm"
						className="h-7 gap-1.5 text-xs"
						onClick={() => openAgentTerminal('antigravity', 'antigravity')}
					>
						<Play className="h-3 w-3" /> Launch Antigravity
					</Button>
				</div>

				<div className="flex items-center justify-between px-4 py-3">
					<div className="space-y-0.5">
						<div className="flex items-center gap-2 text-sm font-medium text-foreground">
							<Bot className="h-4 w-4 text-primary" /> OpenAI Codex CLI
						</div>
						<div className="text-xs text-muted-foreground">Interactive Codex coding terminal.</div>
					</div>
					<Button
						variant="outline"
						size="sm"
						className="h-7 gap-1.5 text-xs"
						onClick={() => openAgentTerminal('codex', 'codex')}
					>
						<Play className="h-3 w-3" /> Launch Codex
					</Button>
				</div>

				<div className="flex items-center justify-between px-4 py-3">
					<div className="space-y-0.5">
						<div className="flex items-center gap-2 text-sm font-medium text-foreground">
							<Bot className="h-4 w-4 text-primary" /> Gemini CLI
						</div>
						<div className="text-xs text-muted-foreground">Interactive Google Gemini CLI session.</div>
					</div>
					<Button
						variant="outline"
						size="sm"
						className="h-7 gap-1.5 text-xs"
						onClick={() => openAgentTerminal('gemini', 'gemini')}
					>
						<Play className="h-3 w-3" /> Launch Gemini
					</Button>
				</div>

				<div className="divide-y divide-border">
					{profiles.map((p) => {
						const isSelected = p.id === selectedProfile.id;
						const isCustom = p.kind === 'custom';
						return (
							<div key={p.id} className="flex items-center justify-between px-4 py-3">
								<div className="space-y-1">
									<div className="flex items-center gap-2">
										<TerminalIcon className="h-4 w-4 text-muted-foreground" />
										<span className="text-sm font-medium text-foreground">{p.label}</span>
										{isSelected && (
											<span className="flex items-center gap-1 rounded-full bg-primary/10 px-2 py-0.5 text-[10px] font-medium text-primary">
												<CheckCircle2 className="h-3 w-3" /> Default
											</span>
										)}
										<span className="rounded bg-muted px-1.5 py-0.5 text-[10px] uppercase tracking-wider text-muted-foreground">
											{p.kind}
										</span>
									</div>
									<div className="font-mono text-xs text-muted-foreground">{p.cmd.join(' ')}</div>
								</div>

								<div className="flex items-center gap-2">
									{!isSelected && (
										<Button
											variant="outline"
											size="sm"
											className="h-7 text-xs"
											onClick={() => setDefaultProfileId(p.id)}
										>
											Set default
										</Button>
									)}
									{isCustom && (
										<Button
											variant="ghost"
											size="sm"
											className="h-7 text-xs text-destructive hover:bg-destructive/10"
											onClick={() => removeCustomProfile(p.id)}
										>
											<Trash2 className="h-3 w-3" />
										</Button>
									)}
									<Button
										variant="ghost"
										size="sm"
										className="h-7 gap-1 text-xs"
										onClick={() => openTestTerminal(p.cmd, p.label)}
									>
										<Play className="h-3 w-3" /> Open
									</Button>
								</div>
							</div>
						);
					})}
				</div>

				{isAddingCustom ? (
					<div className="space-y-3 border-t border-border bg-muted/30 p-4">
						<div className="text-xs font-semibold text-foreground">Add custom shell profile</div>
						<div className="grid grid-cols-1 gap-3 md:grid-cols-2">
							<input
								type="text"
								placeholder="Profile Name (e.g. MSYS2 Bash, Python Venv)"
								value={customLabel}
								onChange={(e) => setCustomLabel(e.target.value)}
								className="rounded-md border border-border bg-background px-3 py-1.5 text-xs text-foreground outline-none focus:ring-1 focus:ring-primary"
							/>
							<input
								type="text"
								placeholder="Command & Args (e.g. C:\msys64\usr\bin\bash.exe -l)"
								value={customCommand}
								onChange={(e) => setCustomCommand(e.target.value)}
								className="rounded-md border border-border bg-background px-3 py-1.5 text-xs text-foreground outline-none focus:ring-1 focus:ring-primary"
							/>
						</div>
						<div className="flex items-center justify-end gap-2">
							<Button variant="ghost" size="sm" className="h-7 text-xs" onClick={() => setIsAddingCustom(false)}>
								Cancel
							</Button>
							<Button
								variant="default"
								size="sm"
								className="h-7 text-xs"
								onClick={handleSaveCustomProfile}
								disabled={!customLabel.trim() || !customCommand.trim()}
							>
								Save profile
							</Button>
						</div>
					</div>
				) : (
					<div className="flex justify-end border-t border-border p-3">
						<Button
							variant="outline"
							size="sm"
							className="h-7 gap-1.5 text-xs"
							onClick={() => setIsAddingCustom(true)}
						>
							<Plus className="h-3.5 w-3.5" /> Add custom shell
						</Button>
					</div>
				)}
			</div>
		</section>
	);
}

export const Route = createFileRoute('/settings/engines')({
	component: EnginesPage,
});
