import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import { Sparkles } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { useShellStore } from '@/lib/shell/shell-store';

import { ActivityBarSectionBody } from './-components/activity-bar-body';
import { ArtifactGridSectionBody } from './-components/artifact-grid-body';

function WorkspacePage() {
	const navigate = useNavigate();
	const resetOnboarding = useShellStore((s) => s.resetOnboarding);

	async function runConsecrationAgain() {
		const ok = await confirmDialog(
			'Reset onboarding and re-run every step? Your existing workspace settings stay put — this only re-opens the wizard.',
			{ title: 'Run consecration again', kind: 'info' }
		);
		if (!ok) return;
		resetOnboarding();
		void navigate({ to: '/onboarding' });
	}

	return (
		<div className="mx-auto w-full max-w-[720px] space-y-6 px-6 py-6">
			<header className="space-y-1">
				<h2
					className="text-2xl font-semibold tracking-tight"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					Workspace
				</h2>
				<p className="max-w-prose text-sm leading-relaxed text-muted-foreground">
					Rail pins, artifact grid behaviour, and the consecration wizard.
				</p>
			</header>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Activity bar pins
					</h3>
				</header>
				<ActivityBarSectionBody />
			</section>

			<ArtifactGridSectionBody />

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Consecration
					</h3>
				</header>
				<div className="flex items-center justify-between gap-4 px-4 py-3">
					<div className="min-w-0">
						<div className="text-sm font-medium text-foreground">Run consecration again</div>
						<div className="text-xs leading-relaxed text-muted-foreground">
							Re-opens the first-run wizard. Existing settings stay put.
						</div>
					</div>
					<Button variant="outline" size="sm" onClick={() => void runConsecrationAgain()}>
						<Sparkles className="mr-1 h-3.5 w-3.5" />
						Run again
					</Button>
				</div>
			</section>
		</div>
	);
}

export const Route = createFileRoute('/settings/workspace')({
	component: WorkspacePage,
});
