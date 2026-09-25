import { createFileRoute, useNavigate } from '@tanstack/react-router';
import { Sparkles } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { writeSettingsField } from '@/lib/settings/client';
import { isDailyAddressEnabled } from '@/lib/settings/daily-address';
import { runConsecrationAgain } from '@/shell/onboarding/run-again';
import { SettingsFieldRow, useSettingsSection } from '@/shell/settings/field';

import { ActivityBarSectionBody } from './-components/activity-bar-body';
import { ArtifactGridSectionBody } from './-components/artifact-grid-body';

/** WP-39 / D-04: `workspace.dailyAddress` — personal-only, default on. */
function DailyAddressSectionBody() {
	const { result, refresh } = useSettingsSection();
	const enabled = isDailyAddressEnabled(result?.effective);

	async function setEnabled(value: boolean) {
		await writeSettingsField({ scope: 'personal', field: 'workspace.dailyAddress', value });
		refresh();
	}

	return (
		<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
			<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
				<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
					Daily address
				</h3>
			</header>
			<SettingsFieldRow
				field="workspace.dailyAddress"
				label="Show the daily address"
				desc="The day-start summary at the top of the Project dashboard: recent runs, what is waiting on you, updates and todos. Personal setting."
			>
				<Switch
					aria-label="Show the daily address"
					checked={enabled}
					onCheckedChange={(v) => void setEnabled(v)}
				/>
			</SettingsFieldRow>
		</section>
	);
}

function WorkspacePage() {
	const navigate = useNavigate();

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
					Rail pins, artifact grid behaviour, the daily address, and the consecration wizard.
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

			<DailyAddressSectionBody />

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
					<Button variant="outline" size="sm" onClick={() => void runConsecrationAgain(() => void navigate({ to: '/onboarding' }))}>
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
