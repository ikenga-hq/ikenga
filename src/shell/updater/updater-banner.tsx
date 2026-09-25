// WP-41 — D-07 `update-flow`, the shell (app-binary) source.
// plans/shell-ux-rearchitecture/drafts/design-spec-D-03-07.md §D-07;
// designs/system-flows.html?state=update-flow (steps `Available` / `Release
// notes` / `Downloading` / `Restart to finish` / `Updated`).
//
// Banner → release-notes sheet → download progress (shared with the status
// bar and the sheet via `updater-store.ts`) → "Restart to finish" with a
// running-sessions count → post-restart toast. The app never relaunches
// itself (updater.ts); the × is "Defer for 24h", per-version (unchanged from
// the shipped banner).
//
// Absorbed from `src/shell/updater-banner.tsx` (G-54: that was the brief's
// stated path already before this WP; unchanged here). The sheet and the
// package-updates variant live in `update-sheet.tsx` — this file only owns
// the banner strip + wiring the shared sheet/restart-marker state.

import { Download, RefreshCw } from 'lucide-react';
import { useEffect } from 'react';
import { useShallow } from 'zustand/react/shallow';
import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useShellStore } from '@/lib/shell/shell-store';
import { useUpdateSheetStore } from '@/lib/updater/sheet-store';
import { markPendingRestart } from '@/lib/updater/post-restart';
import { progressPct } from '@/lib/updater/updater-store';
import { useLiveSessionCount } from '@/lib/updater/restart-sessions';
import { useUpdater } from '@/lib/updater/use-updater';
import { useUpdaterSnooze } from '@/lib/updater/snooze';

function plural(n: number, one: string, many = `${one}s`): string {
	return `${n} ${n === 1 ? one : many}`;
}

export function UpdaterBanner() {
	const autoCheck = useShellStore((s) => s.updatesAutoCheck);
	const autoInstallApp = useShellStore((s) => s.updatesAutoInstallApp);
	const { available, installing, installed, bytesDownloaded, totalBytes, error, install, restart } =
		useUpdater({
			enabled: autoCheck,
		});
	const snooze = useUpdaterSnooze();
	const isSnoozed = snooze.isSnoozed(available?.version ?? null);
	const openSheet = useUpdateSheetStore((s) => s.openSheet);
	const liveSessions = useLiveSessionCount();

	// Opt-in (default off): when `updates.autoInstallApp` is on, a detected
	// binary update downloads + installs without a click — but it still stops
	// at the "installed, restart to finish" banner below. Nothing relaunches
	// the app on its own; a surprise restart discards whatever is live in
	// terminals and pkg panes. Snooze still wins as the escape hatch;
	// `!installing && !installed` stops it re-firing once a download is in
	// flight or already installed.
	useEffect(() => {
		if (autoInstallApp && available && !isSnoozed && !installing && !installed && !error) {
			void install();
		}
	}, [autoInstallApp, available, isSnoozed, installing, installed, error, install]);
	// Hide on /settings/about — the user is already on the dedicated surface
	// for shell updates, so the banner is redundant noise there.
	const onAboutPage = usePaneStore(
		useShallow((s) => {
			const leaf = findLeaf(s.root, s.focusedId);
			if (!leaf) return false;
			const tab = leaf.tabs[leaf.activeTabIdx];
			if (!tab || tab.kind !== 'route') return false;
			return tab.path.split('?')[0] === '/settings/about';
		})
	);

	function doRestart() {
		if (available) {
			markPendingRestart({
				version: available.version,
				notes: available.notes ?? '',
				sessionsBefore: liveSessions,
			});
		}
		void restart();
	}

	if (onAboutPage) return null;
	if (!available && !error) return null;
	// A snoozed update still surfaces once it's installed — the restart is the
	// only thing left and shouldn't be silenced.
	if (available && isSnoozed && !installed) return null;

	// Installed, awaiting restart. Show a deliberate Restart action instead of
	// relaunching out from under the user.
	if (installed) {
		return (
			<Banner
				data-state="update-restart"
				tone="info"
				icon={<RefreshCw />}
				actions={
					<>
						<button
							type="button"
							onClick={() => openSheet('shell')}
							className="font-mono text-[11px] text-muted-foreground hover:text-foreground"
						>
							What's changing →
						</button>
						<Button size="sm" onClick={doRestart}>
							Restart now
						</Button>
					</>
				}
			>
				<span className="font-medium">Ikenga {available?.version}</span>
				<span className="text-muted-foreground"> is installed — restart to finish updating.</span>
				{liveSessions > 0 && (
					<span className="text-muted-foreground">
						{' '}
						· {plural(liveSessions, 'live session')} will be resumed
					</span>
				)}
			</Banner>
		);
	}

	if (error) {
		return (
			<Banner
				data-state="update-error"
				tone="danger"
				icon={<RefreshCw />}
				actions={
					<button
						type="button"
						onClick={() => openSheet('shell')}
						className="font-mono text-[11px] text-muted-foreground hover:text-foreground"
					>
						Details →
					</button>
				}
			>
				Update failed: {error}
			</Banner>
		);
	}

	const pct = progressPct(bytesDownloaded, totalBytes);

	return (
		<Banner
			data-state={installing ? 'update-downloading' : 'update-available'}
			tone="info"
			icon={<Download />}
			onDismiss={installing ? undefined : () => snooze.snooze(available!.version)}
			dismissLabel="Defer for 24h"
			actions={
				<>
					<button
						type="button"
						onClick={() => openSheet('shell')}
						className="font-mono text-[11px] text-muted-foreground hover:text-foreground"
					>
						Release notes →
					</button>
					<Button
						size="sm"
						onClick={() => {
							openSheet('shell');
							void install();
						}}
						disabled={installing}
					>
						{installing ? 'Installing…' : 'Update now'}
					</Button>
				</>
			}
		>
			<span className="font-medium">Ikenga {available!.version}</span>
			<span className="text-muted-foreground"> is available.</span>
			{installing && pct !== null && (
				<span className="text-muted-foreground"> Downloading {pct}%…</span>
			)}
			{installing && pct === null && <span className="text-muted-foreground"> Downloading…</span>}
		</Banner>
	);
}
