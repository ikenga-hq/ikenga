import { useState } from 'react';
import { createFileRoute } from '@tanstack/react-router';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { Terminal, RefreshCw, CheckCircle2, Play, Plus, Trash2, Bot } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { isWindows } from '@/lib/platform';
import { usePaneStore } from '@/lib/panes/pane-store';
import { createTerminalSession } from '@/terminal/single-terminal';
import { buildAgentWrappedCmd, type AgentEngineKind } from '@/terminal/claude-wrap';
import {
	useDefaultShellProfile,
	useShellProfiles,
	useCustomShellProfiles,
	AGENT_ENV_KEY,
	AGENT_WSL_DISTRO_KEY,
} from '@/lib/shell-profiles';
import { settingsGet, settingsSet } from '@/lib/tauri-cmd';
import { SettingGroup } from './-components/setting-group';
import { SettingRow } from './-components/setting-row';

function TerminalSettingsPage() {
	const queryClient = useQueryClient();
	const { profiles, selectedProfile, setDefaultProfileId, isLoading } = useDefaultShellProfile();
	const { refetch: refetchProfiles, isFetching } = useShellProfiles();
	const { customProfiles, addCustomProfile, removeCustomProfile } = useCustomShellProfiles();

	const [isAddingCustom, setIsAddingCustom] = useState(false);
	const [customLabel, setCustomLabel] = useState('');
	const [customCommand, setCustomCommand] = useState('');

	const agentEnvQuery = useQuery<string>({
		queryKey: ['settings', AGENT_ENV_KEY],
		queryFn: async () => {
			const res = await settingsGet(AGENT_ENV_KEY);
			return res ?? 'native';
		},
	});

	const agentWslDistroQuery = useQuery<string | null>({
		queryKey: ['settings', AGENT_WSL_DISTRO_KEY],
		queryFn: async () => {
			return await settingsGet(AGENT_WSL_DISTRO_KEY);
		},
	});

	const agentEnvMutation = useMutation({
		mutationFn: async (envKind: string) => {
			await settingsSet(AGENT_ENV_KEY, envKind);
		},
		onSuccess: () => {
			void queryClient.invalidateQueries({ queryKey: ['settings', AGENT_ENV_KEY] });
		},
	});

	const agentDistroMutation = useMutation({
		mutationFn: async (distro: string) => {
			await settingsSet(AGENT_WSL_DISTRO_KEY, distro);
		},
		onSuccess: () => {
			void queryClient.invalidateQueries({ queryKey: ['settings', AGENT_WSL_DISTRO_KEY] });
		},
	});

	const wslProfiles = profiles.filter((p) => p.kind === 'wsl');
	const currentAgentEnv = agentEnvQuery.data ?? 'native';
	const currentAgentDistro =
		agentWslDistroQuery.data ??
		(wslProfiles.length > 0 ? (wslProfiles[0].distro ?? 'default') : null);

	function openTestTerminal(cmd: string[], title: string) {
		const focusedId = usePaneStore.getState().focusedId;
		const sessionId = createTerminalSession({ cmd, title });
		usePaneStore.getState().addTab(focusedId, { kind: 'terminal', sessionId });
	}

	function openAgentTerminal(engine: AgentEngineKind, title: string) {
		const isWsl = currentAgentEnv === 'wsl';
		const cmd = buildAgentWrappedCmd({
			engine,
			shellTarget: isWsl ? 'wsl' : isWindows ? 'native' : 'posix',
			wslDistro: isWsl ? currentAgentDistro : undefined,
		});
		openTestTerminal(cmd, title);
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
		<div className="flex h-full flex-col">
			<div className="flex h-10 shrink-0 items-center justify-between border-b border-border-soft px-6 text-xs text-muted-foreground">
				<span>
					Settings · <span className="font-semibold text-foreground">Terminal & Shells</span>
				</span>
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
			</div>

			<div className="flex-1 overflow-y-auto px-6 py-6">
				<div className="mx-auto max-w-3xl space-y-6">
					<header className="space-y-1">
						<h2
							className="text-2xl font-semibold tracking-tight"
							style={{ fontFamily: 'var(--font-display)' }}
						>
							Terminal & Shells
						</h2>
						<p className="max-w-prose text-sm leading-relaxed text-muted-foreground">
							Configure default shells, manage detected environments (PowerShell, WSL, Git Bash,
							CMD), define custom shell profiles, and launch AI agent terminal sessions.
						</p>
					</header>

					<SettingGroup title="Default Shell">
						<SettingRow
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
						</SettingRow>
					</SettingGroup>

					{isWindows && (
						<SettingGroup title="Coding Agent Environment">
							<SettingRow
								label="Agent Execution Target"
								desc="Select whether coding agents like Claude Code CLI run inside Native Windows or inside a WSL Linux environment."
							>
								<div className="flex items-center gap-3">
									<label className="flex items-center gap-1.5 text-xs cursor-pointer">
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
										<label className="flex items-center gap-1.5 text-xs cursor-pointer">
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
							</SettingRow>

							{currentAgentEnv === 'wsl' && wslProfiles.length > 0 && (
								<SettingRow
									label="WSL Distribution"
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
								</SettingRow>
							)}
						</SettingGroup>
					)}

					<SettingGroup title="AI Agent Terminal Launchers">
						<div className="divide-y divide-border">
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
									<div className="text-xs text-muted-foreground">
										Interactive Codex coding terminal.
									</div>
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
									<div className="text-xs text-muted-foreground">
										Interactive Google Gemini CLI session.
									</div>
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
						</div>
					</SettingGroup>

					<SettingGroup title="Detected & Custom Shell Profiles">
						<div className="divide-y divide-border">
							{profiles.map((p) => {
								const isSelected = p.id === selectedProfile.id;
								const isCustom = p.kind === 'custom';
								return (
									<div key={p.id} className="flex items-center justify-between px-4 py-3">
										<div className="space-y-1">
											<div className="flex items-center gap-2">
												<Terminal className="h-4 w-4 text-muted-foreground" />
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
											<div className="font-mono text-xs text-muted-foreground">
												{p.cmd.join(' ')}
											</div>
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
							<div className="border-t border-border bg-muted/30 p-4 space-y-3">
								<div className="text-xs font-semibold text-foreground">
									Add Custom Shell Profile
								</div>
								<div className="grid grid-cols-1 md:grid-cols-2 gap-3">
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
									<Button
										variant="ghost"
										size="sm"
										className="h-7 text-xs"
										onClick={() => setIsAddingCustom(false)}
									>
										Cancel
									</Button>
									<Button
										variant="default"
										size="sm"
										className="h-7 text-xs"
										onClick={handleSaveCustomProfile}
										disabled={!customLabel.trim() || !customCommand.trim()}
									>
										Save Profile
									</Button>
								</div>
							</div>
						) : (
							<div className="border-t border-border p-3 flex justify-end">
								<Button
									variant="outline"
									size="sm"
									className="h-7 gap-1.5 text-xs"
									onClick={() => setIsAddingCustom(true)}
								>
									<Plus className="h-3.5 w-3.5" /> Add Custom Shell
								</Button>
							</div>
						)}
					</SettingGroup>
				</div>
			</div>
		</div>
	);
}

export const Route = createFileRoute('/settings/terminal')({
	component: TerminalSettingsPage,
});
