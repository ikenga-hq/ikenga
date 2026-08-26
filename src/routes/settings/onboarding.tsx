// Settings → Onboarding — lists every wizard step + lets the user re-run
// individual steps or restart from scratch. The re-run flow enters the
// wizard in `mode: 'edit'`, which the chrome hints at with a small badge.
//
// Phase 3 scaffold — Phase 4 doesn't really change this surface beyond
// possibly enriching the per-step summary text from the step payloads.

import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import { Pencil, RotateCcw } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import {
	ONBOARDING_STEPS,
	type OnboardingStatus,
	type OnboardingStepId,
	type OnboardingStepRecord,
	useShellStore,
} from '@/lib/shell/shell-store';

import { SettingGroup } from './-components/setting-group';

const STEP_LABELS: Record<OnboardingStepId, string> = {
	welcome: 'Welcome',
	agent: 'Coding agent',
	roots: 'Project roots',
	packages: 'Packages',
	connectors: 'Connectors',
	scaffolding: '.claude/ scaffold',
	appearance: 'Appearance',
	summary: 'Summary',
};

const STATUS_BADGE_COPY: Record<OnboardingStatus, string> = {
	pending: 'Pending',
	in_progress: 'In progress',
	completed: 'Completed',
	skipped: 'Skipped',
};

function OnboardingSettingsPage() {
	const navigate = useNavigate();
	const onboarding = useShellStore((s) => s.onboarding);
	const enterOnboardingEdit = useShellStore((s) => s.enterOnboardingEdit);
	const resetOnboarding = useShellStore((s) => s.resetOnboarding);

	async function handleStartOver() {
		const ok = await confirmDialog(
			'Reset onboarding and re-run every step? Your existing workspace settings stay put — this only re-opens the wizard.',
			{ title: 'Start onboarding over', kind: 'warning' }
		);
		if (!ok) return;
		resetOnboarding();
		void navigate({ to: '/onboarding' });
	}

	function handleRerun(stepId: OnboardingStepId) {
		enterOnboardingEdit(stepId);
		void navigate({ to: `/onboarding/${stepId}` });
	}

	return (
		<div className="flex h-full flex-col">
			<div className="flex h-10 shrink-0 items-center gap-3 border-b border-border-soft px-6 text-xs text-muted-foreground">
				<span>
					Settings · <span className="font-semibold text-foreground">Onboarding</span>
				</span>
			</div>

			<div className="flex-1 overflow-y-auto px-6 py-6">
				<div className="mx-auto max-w-3xl space-y-6">
					<header className="space-y-1">
						<h2
							className="text-2xl font-semibold tracking-tight"
							style={{ fontFamily: 'var(--font-display)' }}
						>
							Onboarding
						</h2>
						<p className="max-w-prose text-sm leading-relaxed text-muted-foreground">
							The first-run wizard sets up your agent, project roots, packages and connectors. You
							can re-run any step here — re-running opens the wizard in edit mode and returns you to
							Settings afterwards.
						</p>
					</header>

					<SettingGroup title="Wizard steps">
						<ul className="divide-y divide-border">
							{ONBOARDING_STEPS.map((id) => {
								const record = onboarding.steps[id];
								return (
									<li key={id} className="flex items-center gap-3 px-4 py-3">
										<div className="min-w-0 flex-1">
											<div className="flex items-center gap-2">
												<span className="text-sm font-medium text-foreground">
													{STEP_LABELS[id]}
												</span>
												<StatusBadge status={record.status} />
											</div>
											<div className="mt-0.5 font-mono text-[11px] text-muted-foreground">
												{formatStepMeta(record)}
											</div>
										</div>
										<Button
											variant="outline"
											size="sm"
											onClick={() => handleRerun(id)}
											data-testid={`settings-onboarding-rerun-${id}`}
										>
											<Pencil className="mr-1 h-3.5 w-3.5" />
											Re-run
										</Button>
									</li>
								);
							})}
						</ul>
					</SettingGroup>

					<SettingGroup title="Danger zone">
						<div className="flex items-center justify-between gap-4 px-4 py-3">
							<div className="min-w-0">
								<div className="text-sm font-medium text-foreground">Start onboarding over</div>
								<div className="text-xs leading-relaxed text-muted-foreground">
									Resets every step to pending and re-opens the wizard in first-run mode. Your
									existing settings (theme, file roots, installed packages) are not touched.
								</div>
							</div>
							<Button
								variant="outline"
								size="sm"
								onClick={handleStartOver}
								className="text-red-700"
								data-testid="settings-onboarding-start-over"
							>
								<RotateCcw className="mr-1 h-3.5 w-3.5" />
								Start over
							</Button>
						</div>
					</SettingGroup>
				</div>
			</div>
		</div>
	);
}

function StatusBadge({ status }: { status: OnboardingStatus }) {
	return (
		<span
			className={cn(
				'inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wider',
				status === 'completed' && 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300',
				status === 'skipped' && 'bg-amber-500/10 text-amber-700 dark:text-amber-300',
				status === 'in_progress' && 'bg-sky-500/10 text-sky-700 dark:text-sky-300',
				status === 'pending' && 'bg-muted text-muted-foreground'
			)}
		>
			{STATUS_BADGE_COPY[status]}
		</span>
	);
}

function formatStepMeta(record: OnboardingStepRecord): string {
	if (record.completedAt) {
		const d = new Date(record.completedAt);
		return `last edited ${d.toLocaleString()}`;
	}
	return 'not yet run';
}

export const Route = createFileRoute('/settings/onboarding')({
	component: OnboardingSettingsPage,
});
