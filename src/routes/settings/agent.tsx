// Settings → Agent — current coding-agent selection + a "Change agent"
// shortcut that re-enters the onboarding wizard at the agent step in edit
// mode. The wizard step (`shell/onboarding/agent-body.tsx`) is the source
// of truth for the picker UI; this page surfaces the persisted choice and
// the auth-status banner so users can see at a glance whether their engine
// is healthy without re-running detection.

import { useQuery } from '@tanstack/react-query';
import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { openExternalUrl } from '@/lib/transport/shims';
import { AlertTriangle, Bot, Pencil, RotateCcw } from 'lucide-react';

import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import { StatusChip } from '@/components/ui/status-chip';
import { useShellStore } from '@/lib/shell/shell-store';
import { type DetectedAgent, detectAgents } from '@/lib/tauri-cmd';

import { SettingGroup } from './-components/setting-group';
import { SettingRow } from './-components/setting-row';

const OFFLINE_AGENT_ID = 'engine-noop';

function AgentSettingsPage() {
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
		enterOnboardingEdit('agent');
		void navigate({ to: '/onboarding/agent' });
	}

	return (
		<div className="flex h-full flex-col">
			<div className="flex h-10 shrink-0 items-center gap-3 border-b border-border-soft px-6 text-xs text-muted-foreground">
				<span>
					Settings · <span className="font-semibold text-foreground">Agent</span>
				</span>
			</div>

			<div className="flex-1 overflow-y-auto px-6 py-6">
				<div className="mx-auto max-w-3xl space-y-6">
					<header className="space-y-1">
						<h2
							className="text-2xl font-semibold tracking-tight"
							style={{ fontFamily: 'var(--font-display)' }}
						>
							Coding agent
						</h2>
						<p className="max-w-prose text-sm leading-relaxed text-muted-foreground">
							Which engine drives terminal sessions. Change here re-opens the onboarding picker so
							you can rescan your machine and pick another agent.
						</p>
					</header>

					<SettingGroup title="Current selection">
						<SettingRow
							label="Engine"
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
						</SettingRow>

						{!isOffline && (
							<>
								<SettingRow
									label="Auth status"
									desc="Whether the selected agent is currently signed in / has a working API key."
								>
									<AuthBadge authed={authed} loading={isLoading} />
								</SettingRow>
								<SettingRow label="Binary" desc="Path the agent CLI was discovered at.">
									<span
										className="truncate font-mono text-[11px] text-muted-foreground"
										title={execPath}
									>
										{execPath ?? '(unknown)'}
									</span>
								</SettingRow>
								{version && (
									<SettingRow label="Version" desc="Reported by the agent's --version probe.">
										<span className="font-mono text-[11px] text-muted-foreground">{version}</span>
									</SettingRow>
								)}
							</>
						)}

						<SettingRow
							label="Default agent id"
							desc="The pkg id wired as the default terminal agent. `null` when offline."
						>
							<span className="font-mono text-[11px] text-muted-foreground">
								{defaultEngineId ?? '(none)'}
							</span>
						</SettingRow>
					</SettingGroup>

					<SettingGroup title="Change agent">
						<div className="flex items-center justify-between gap-4 px-4 py-3">
							<div className="min-w-0">
								<div className="text-sm font-medium text-foreground">Re-run the picker</div>
								<div className="text-xs leading-relaxed text-muted-foreground">
									Opens the onboarding agent step in edit mode. Comes back here when you save.
								</div>
							</div>
							<div className="flex gap-2">
								<Button variant="outline" size="sm" onClick={() => refetch()} disabled={isLoading}>
									<RotateCcw className="mr-1 h-3.5 w-3.5" />
									Re-scan
								</Button>
								<Button variant="default" size="sm" onClick={handleChange}>
									<Pencil className="mr-1 h-3.5 w-3.5" />
									Change agent
								</Button>
							</div>
						</div>
					</SettingGroup>

					{authed === false && live && (
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
							<div className="text-[13px] font-semibold">{live.display} isn't signed in</div>
							<div className="mt-1 text-xs" style={{ color: 'var(--fg-muted)' }}>
								{live.auth_hint ??
									'Run the agent CLI once to authenticate, or set the relevant API key in your environment.'}
							</div>
						</Banner>
					)}
				</div>
			</div>
		</div>
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

export const Route = createFileRoute('/settings/agent')({
	component: AgentSettingsPage,
});
