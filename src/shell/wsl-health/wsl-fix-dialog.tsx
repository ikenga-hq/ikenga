// honest-failure-states WP-2 — the confirm in front of the two fixes that
// shut WSL down (D-1, D-5, D-6). Mounted once (next to the notification toast
// bridge in `notifications/bell.tsx`), so the banner, Settings › Engines and
// a notification button all reach the same dialog via `requestWslFix`.
// The fix flow (terminal store, Tauri) loads lazily on confirm, so the
// status bar doesn't pull it in.

import { useState } from 'react';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { type WslFixConfirm, useWslHealthUi } from '@/lib/wsl-health/store';
import { wslDistroLabel } from '@/lib/wsl-health/tabs';

/** Where `switch_to_nat` puts the backup (`wsl_health.rs`). */
export const WSLCONFIG_BACKUP_HINT = '%UserProfile%\\.wslconfig.bak-<date>-<time>';

export function WslFixConfirmBody({ confirm }: { confirm: WslFixConfirm }) {
	const sessions = confirm.sessions.filter((s) => s.wasRunning);
	return (
		<div className="space-y-3 text-sm text-muted-foreground">
			{confirm.action === 'switch_to_nat' ? (
				<>
					<p>
						WSL's mirrored networking keeps failing to start on this PC. NAT networking is WSL's
						older, simpler mode and usually works where mirrored doesn't.
					</p>
					<p>
						<span className="font-medium text-foreground">The trade-off:</span> with NAT, WSL gets
						its own private address. Servers you run inside WSL (dev servers, SSH, …) stop being
						reachable at this PC's LAN or Tailscale address unless you set up port forwarding.
						Windows apps on this PC can still reach them at <code>localhost</code>.
					</p>
					<p>
						Ikenga sets <code>networkingMode=nat</code> in <code>.wslconfig</code> (other lines and
						comments are kept; if the file doesn't exist yet, it is created), and restarts WSL. If
						you already have a <code>.wslconfig</code>, it is first backed up to{' '}
						<code className="break-all">{WSLCONFIG_BACKUP_HINT}</code>.
					</p>
				</>
			) : (
				<p>
					Windows will ask for administrator approval. Ikenga then shuts WSL down (every
					distribution) and restarts Windows' Host Network Service, which is what brings{' '}
					{wslDistroLabel(confirm.distro)}'s network back. Restarting that service briefly drops
					other virtual networks on this PC too (Docker Desktop, Hyper-V and Windows Sandbox VMs).
				</p>
			)}
			{sessions.length > 0 ? (
				<div>
					<p>
						This closes{' '}
						{sessions.length === 1
							? 'this Ikenga session'
							: `these ${sessions.length} Ikenga sessions`}
						. Ikenga reopens {sessions.length === 1 ? 'it' : 'them'} afterwards and resumes Claude
						conversations where it can:
					</p>
					<ul className="mt-1.5 space-y-0.5" data-wsl-fix-sessions>
						{sessions.map((s) => (
							<li key={s.tabId} className="flex items-center gap-2 text-foreground">
								<span className="truncate">{s.title}</span>
								<span className="font-mono text-[10px] text-muted-foreground">
									{s.distro === 'default' ? 'default distro' : s.distro}
									{s.claudeSessionId ? ' · resumes' : ''}
								</span>
							</li>
						))}
					</ul>
				</div>
			) : (
				<p>No Ikenga WSL sessions are running, so nothing in Ikenga closes.</p>
			)}
		</div>
	);
}

function cancelConfirm(): void {
	useWslHealthUi.getState().setConfirm(null);
}

export function WslFixDialogHost() {
	const confirm = useWslHealthUi((s) => s.confirm);
	const [busy, setBusy] = useState(false);
	const title =
		confirm?.action === 'switch_to_nat'
			? 'Switch WSL to NAT networking?'
			: 'Restart WSL networking?';
	const cta =
		confirm?.action === 'switch_to_nat' ? 'Back up and switch to NAT' : 'Restart WSL networking';
	return (
		<Dialog
			open={confirm !== null}
			onOpenChange={(open) => {
				if (!open && !busy) cancelConfirm();
			}}
		>
			{confirm && (
				<DialogContent data-wsl-fix-confirm={confirm.action} showCloseButton={false}>
					<DialogHeader>
						<DialogTitle>{title}</DialogTitle>
						<DialogDescription asChild>
							<div>
								<WslFixConfirmBody confirm={confirm} />
							</div>
						</DialogDescription>
					</DialogHeader>
					<DialogFooter>
						<Button variant="ghost" size="sm" disabled={busy} onClick={cancelConfirm}>
							Cancel
						</Button>
						<Button
							size="sm"
							disabled={busy}
							aria-busy={busy || undefined}
							onClick={() => {
								setBusy(true);
								// The dialog closes as soon as the fix starts; progress
								// and the outcome show on the banner / settings row.
								void import('@/lib/wsl-health/fix-flow')
									.then((m) => m.confirmWslFix())
									.catch((err) => console.error('[wsl-health] fix failed', err))
									.finally(() => setBusy(false));
							}}
						>
							{cta}
						</Button>
					</DialogFooter>
				</DialogContent>
			)}
		</Dialog>
	);
}
