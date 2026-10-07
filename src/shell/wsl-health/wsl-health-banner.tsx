// honest-failure-states WP-2 (D-2, D-8) — the banner a WSL terminal pane
// shows while WSL's network is broken: the cause in plain language, the fix
// that fits (copy.ts), "Check again", and dismiss-for-this-episode. Passive:
// it shows what the pre-launch probe or the PTY errno scanner measured and
// never probes on its own (D-7). Every affected pane shows it, independent of
// the one-per-episode notification.

import { Loader2, RefreshCw, WifiOff } from 'lucide-react';
import { useState } from 'react';
import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import type { WslFixAction, WslHealth } from '@/lib/tauri-cmd';
import { type WslFixChoice, wslHealthCopy } from '@/lib/wsl-health/copy';
import { requestWslFix } from '@/lib/wsl-health/fix-flow';
import { probeWslHealth, useWslHealth } from '@/lib/wsl-health/query';
import { useWslHealthUi, type WslFixRun } from '@/lib/wsl-health/store';
import { isWslTab, wslTabDistro } from '@/lib/wsl-health/tabs';
import { useTerminalStore } from '@/terminal/session-store';

export interface WslHealthBannerViewProps {
	health: WslHealth;
	run: WslFixRun | null;
	restartTried?: boolean;
	checking?: boolean;
	onFix: (action: WslFixAction) => void;
	onCheck: () => void;
	onDismiss: () => void;
}

const BUSY: Record<string, string> = {
	running: 'Working on it…',
	relaunching: 'Reopening WSL sessions…',
};

/** Presentational half — rendered per state in tests. `null` when healthy. */
export function WslHealthBannerView({
	health,
	run,
	restartTried,
	checking,
	onFix,
	onCheck,
	onDismiss,
}: WslHealthBannerViewProps) {
	const copy = wslHealthCopy(health, { restartTried });
	if (!copy) return null;
	const busy = run?.phase === 'running' || run?.phase === 'relaunching';
	// A fix's outcome replaces the explanation until a newer probe result
	// arrives (a later episode must not show an old "Fixed").
	const outcome = !busy && run?.message && run.at >= health.checkedAt ? run.message : null;
	const fixButton = (choice: WslFixChoice, primary: boolean) => (
		<Button
			key={choice.action}
			size="xs"
			variant={primary ? 'default' : 'ghost'}
			disabled={busy}
			onClick={() => onFix(choice.action)}
		>
			{choice.label}
		</Button>
	);
	return (
		<Banner
			tone={copy.tone}
			icon={<WifiOff />}
			role={copy.tone === 'danger' ? 'alert' : 'status'}
			className="px-3 py-1.5 text-xs"
			data-wsl-health={health.state}
			onDismiss={onDismiss}
			dismissLabel="Hide until WSL is back online"
			actions={
				<>
					{copy.primary && fixButton(copy.primary, true)}
					{copy.secondary.map((c) => fixButton(c, false))}
					<Button
						size="xs"
						variant="ghost"
						disabled={busy || checking}
						onClick={onCheck}
						aria-label="Check WSL network again"
					>
						<RefreshCw className={checking ? 'animate-spin' : undefined} />
						Check again
					</Button>
				</>
			}
		>
			<div className="font-medium">{copy.title}</div>
			<div className="text-muted-foreground">
				{busy ? (
					<span className="inline-flex items-center gap-1">
						<Loader2 className="size-3 animate-spin" aria-hidden />
						{BUSY[run?.phase ?? 'running']}
					</span>
				) : (
					(outcome ?? copy.body)
				)}
			</div>
		</Banner>
	);
}

/** The pane banner for terminal `sessionId`; renders nothing for non-WSL
 *  tabs, healthy WSL, or a pane that dismissed this episode. */
export function WslHealthBanner({ sessionId }: { sessionId: string }) {
	const tab = useTerminalStore((s) => s.tabs.find((t) => t.id === sessionId));
	const wsl = tab ? isWslTab(tab) : false;
	const distro = tab && wsl ? wslTabDistro(tab) : 'default';
	const { data: health } = useWslHealth(distro, { enabled: false });
	const dismissed = useWslHealthUi((s) => s.dismissed[distro]?.includes(sessionId) ?? false);
	const run = useWslHealthUi((s) => s.runs[distro] ?? null);
	const restartTried = useWslHealthUi((s) => s.restartTried[distro] ?? false);
	const [checking, setChecking] = useState(false);

	if (!wsl || !health || dismissed) return null;
	return (
		<WslHealthBannerView
			health={health}
			run={run}
			restartTried={restartTried}
			checking={checking}
			onFix={(action) => requestWslFix(action, distro)}
			onCheck={() => {
				setChecking(true);
				probeWslHealth(distro, { force: true })
					.catch((err) => console.warn('[wsl-health] check failed', err))
					.finally(() => setChecking(false));
			}}
			onDismiss={() => useWslHealthUi.getState().dismiss(distro, sessionId)}
		/>
	);
}
