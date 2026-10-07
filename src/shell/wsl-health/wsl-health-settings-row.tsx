// honest-failure-states WP-2 (D-2) — Settings › Engines "WSL health" row:
// state chip, the probe's detail, "Check now" and the same fixes as the pane
// banner. Shows what a launch / errno probe already measured and probes only
// on "Check now": opening Settings is not a WSL launch (D-7), and a probe can
// cold-start the distro.

import { Loader2, RefreshCw } from 'lucide-react';
import { useState } from 'react';
import { Button } from '@/components/ui/button';
import { type ChipTone, StatusChip } from '@/components/ui/status-chip';
import type { WslHealthState } from '@/lib/tauri-cmd';
import { wslHealthCopy, wslHealthStateLabel } from '@/lib/wsl-health/copy';
import { requestWslFix } from '@/lib/wsl-health/fix-flow';
import { probeWslHealth, useWslHealth } from '@/lib/wsl-health/query';
import { useWslHealthUi } from '@/lib/wsl-health/store';
import { normalizeWslDistro } from '@/lib/wsl-health/tabs';
import { SettingsFieldRow } from '@/shell/settings/field';

const CHIP_TONE: Record<WslHealthState, ChipTone> = {
	ok: 'live',
	host_offline: 'muted',
	no_route: 'danger',
	dns_only: 'warn',
	wsl_down: 'danger',
	not_installed: 'faint',
};

export function WslHealthSettingsRow({ distro }: { distro: string | null }) {
	const key = normalizeWslDistro(distro);
	const { data: health, error } = useWslHealth(key, { enabled: false });
	const run = useWslHealthUi((s) => s.runs[key] ?? null);
	const restartTried = useWslHealthUi((s) => s.restartTried[key] ?? false);
	const [checking, setChecking] = useState(false);

	const copy = health ? wslHealthCopy(health, { restartTried }) : null;
	const busy = run?.phase === 'running' || run?.phase === 'relaunching';
	const outcome =
		!busy && run?.message && (!health || run.at >= health.checkedAt) ? run.message : null;
	const fixes = copy ? [copy.primary, ...copy.secondary].filter((c) => c !== null) : [];

	return (
		<SettingsFieldRow
			field={null}
			label="WSL health"
			stacked
			desc="Whether the WSL distribution above can reach the internet. Checked before each WSL launch and when a session reports a network error."
		>
			<div className="space-y-2" data-wsl-health-row={health?.state ?? 'unknown'}>
				<div className="flex flex-wrap items-center gap-2">
					{health ? (
						<StatusChip tone={CHIP_TONE[health.state]} dot>
							{wslHealthStateLabel(health.state)}
						</StatusChip>
					) : checking ? (
						<StatusChip tone="muted">Checking…</StatusChip>
					) : error ? (
						<StatusChip tone="muted">Couldn't check</StatusChip>
					) : (
						<StatusChip tone="muted">Not checked yet</StatusChip>
					)}
					<span className="min-w-0 flex-1 text-xs text-muted-foreground">
						{busy ? (
							<span className="inline-flex items-center gap-1">
								<Loader2 className="size-3 animate-spin" aria-hidden />
								{run?.phase === 'relaunching' ? 'Reopening WSL sessions…' : 'Working on it…'}
							</span>
						) : (
							(outcome ??
							copy?.title ??
							health?.detail ??
							(error ? `The check didn't run: ${String(error)}` : null))
						)}
					</span>
				</div>
				<div className="flex flex-wrap items-center gap-2">
					{fixes.map((c, i) => (
						<Button
							key={c.action}
							size="xs"
							variant={i === 0 ? 'default' : 'outline'}
							disabled={busy}
							onClick={() => requestWslFix(c.action, key)}
						>
							{c.label}
						</Button>
					))}
					<Button
						size="xs"
						variant="outline"
						disabled={busy || checking}
						onClick={() => {
							setChecking(true);
							probeWslHealth(key, { force: true })
								.catch((err) => console.warn('[wsl-health] check failed', err))
								.finally(() => setChecking(false));
						}}
					>
						<RefreshCw className={checking ? 'animate-spin' : undefined} />
						Check now
					</Button>
				</div>
			</div>
		</SettingsFieldRow>
	);
}
