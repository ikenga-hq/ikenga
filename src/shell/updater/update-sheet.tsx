// WP-41 — the ONE update sheet: D-07 `update-flow`'s "release notes" +
// "downloading" + "restart to finish" steps for the shell binary, plus the
// package-updates batch variant, as two tabs of the same sheet
// (`designs/system-flows.html?state=update-flow`, `UP.mode: 'shell' | 'pkgs'`
// — "Shell" / "Packages (N)").
//
// Mounted once (`workspace.tsx`), controlled by `sheet-store.ts` so either
// <UpdaterBanner> (shell) or <PkgAutoUpdater> (packages) can open it without
// prop-drilling between the two sibling banners — that's the "two sources"
// the WP-41 brief asks this surface to consolidate.
//
// "Updated" (mockup step 4) has no sheet step here: the sheet's React state
// can't survive `restartApp()` relaunching the process. That step is
// `<PostRestartUpdateToast>` instead, fired from the marker `restart()`
// writes before relaunching (`src/lib/updater/post-restart.ts`).

import { useEffect, useState } from 'react';
import { AlertTriangle, CheckCircle2 } from 'lucide-react';
import { Markdown } from '@/components/markdown';
import { Button } from '@/components/ui/button';
import {
	Sheet,
	SheetContent,
	SheetDescription,
	SheetFooter,
	SheetHeader,
	SheetTitle,
} from '@/components/ui/sheet';
import { cn } from '@/components/ui/utils';
import { usePkgsDerived } from '@/lib/pkgs/use-derived';
import { useUpdatePkgs, type UpdateFailure, type UpdateProgress } from '@/lib/pkgs/use-update-pkgs';
import { progressPct } from '@/lib/updater/updater-store';
import { markPendingRestart } from '@/lib/updater/post-restart';
import { useLiveSessionCount } from '@/lib/updater/restart-sessions';
import { useUpdateSheetStore } from '@/lib/updater/sheet-store';
import { useUpdater } from '@/lib/updater/use-updater';
import { useGitHubReleases, findReleaseByVersion } from '@/lib/updater/use-github-releases';
import { RestartWarning } from './restart-warning';

function plural(n: number, one: string, many = `${one}s`): string {
	return `${n} ${n === 1 ? one : many}`;
}

export function UpdateSheet() {
	const open = useUpdateSheetStore((s) => s.open);
	const source = useUpdateSheetStore((s) => s.source);
	const openSheet = useUpdateSheetStore((s) => s.openSheet);
	const close = useUpdateSheetStore((s) => s.close);
	const pkgs = usePkgsDerived();
	const pkgCount = pkgs.updates.length;

	return (
		<Sheet open={open} onOpenChange={(v) => (v ? openSheet(source) : close())}>
			<SheetContent side="right" className="w-full sm:max-w-md">
				<SheetHeader>
					<SheetTitle>Update</SheetTitle>
					<SheetDescription className="sr-only">
						Shell and package update flow
					</SheetDescription>
				</SheetHeader>
				<div
					role="group"
					aria-label="Which updater"
					className="mx-4 flex h-[26px] w-fit items-stretch overflow-hidden rounded-[var(--radius-sm)] border border-border"
				>
					<button
						type="button"
						onClick={() => openSheet('shell')}
						aria-pressed={source === 'shell'}
						className={cn(
							'border-r border-border px-3 font-mono text-[11px]',
							source === 'shell' ? 'bg-[var(--bg-raised)] text-foreground' : 'text-muted-foreground'
						)}
					>
						Shell
					</button>
					<button
						type="button"
						onClick={() => openSheet('pkgs')}
						aria-pressed={source === 'pkgs'}
						className={cn(
							'px-3 font-mono text-[11px]',
							source === 'pkgs' ? 'bg-[var(--bg-raised)] text-foreground' : 'text-muted-foreground'
						)}
					>
						Packages ({pkgCount})
					</button>
				</div>
				<div className="flex-1 overflow-y-auto px-4 py-3">
					{source === 'shell' ? <ShellUpdatePanel /> : <PkgUpdatePanel />}
				</div>
			</SheetContent>
		</Sheet>
	);
}

/* ───── Shell tab ───── */

function ShellUpdatePanel() {
	// Secondary instance (autoPoll: false) — <UpdaterBanner> already owns the
	// boot check + 6h interval, same convention as `about.tsx`.
	const updater = useUpdater({ autoPoll: false });
	const releases = useGitHubReleases();
	const liveSessions = useLiveSessionCount();

	if (!updater.available) {
		return <p className="text-sm text-muted-foreground">No shell update in progress.</p>;
	}

	const matchingRelease = findReleaseByVersion(releases.data, updater.available.version);
	const notes = matchingRelease?.body ?? updater.available.notes ?? '';

	if (updater.installed) {
		return (
			<div data-state="update-restart" className="space-y-4">
				<h3 className="font-display text-sm font-semibold">
					Installed · restart to finish <span className="font-mono">{updater.available.version}</span>
				</h3>
				<p className="text-sm text-muted-foreground">
					{updater.available.version} is on disk and verified. Ikenga will not relaunch itself — a
					surprise restart discards whatever is live in a terminal or a package pane.
				</p>
				<RestartWarning />
				<SheetFooter className="px-0">
					<Button
						onClick={() => {
							markPendingRestart({
								version: updater.available!.version,
								notes,
								sessionsBefore: liveSessions,
							});
							void updater.restart();
						}}
					>
						Restart now
					</Button>
				</SheetFooter>
			</div>
		);
	}

	if (updater.installing) {
		const pct = progressPct(updater.bytesDownloaded, updater.totalBytes);
		return (
			<div data-state="update-downloading" className="space-y-4">
				<h3 className="font-display text-sm font-semibold">
					Downloading <span className="font-mono">{updater.available.version}</span>
				</h3>
				<div className="space-y-1.5">
					<div className="h-1.5 overflow-hidden rounded-full bg-muted">
						<div
							className="h-full bg-[var(--achievement,var(--warning))] transition-all motion-reduce:transition-none"
							style={{ width: pct !== null ? `${pct}%` : '35%' }}
						/>
					</div>
					<div className="font-mono text-[11px] text-muted-foreground">
						{pct !== null ? `${pct}%` : 'downloading…'}
					</div>
				</div>
				<p className="text-sm text-muted-foreground">
					The progress also runs in the status bar, so you can keep working in a pane while it
					finishes. Nothing is installed until the signature verifies.
				</p>
				<SheetFooter className="px-0">
					<Button
						variant="outline"
						disabled
						title="Cancelling mid-download isn't supported yet"
					>
						Cancel the download
					</Button>
				</SheetFooter>
			</div>
		);
	}

	return (
		<div data-state="update-notes" className="space-y-4">
			<h3 className="font-display text-sm font-semibold">
				What changed in <span className="font-mono">{updater.available.version}</span>
			</h3>
			{notes ? (
				<div className="prose-sm max-w-none rounded-md border border-border bg-background p-3">
					<Markdown content={notes} />
				</div>
			) : (
				<p className="text-sm text-muted-foreground">No release notes published.</p>
			)}
			{updater.error && (
				<p className="text-sm" style={{ color: 'var(--danger)' }}>
					Update failed: {updater.error}
				</p>
			)}
			<p className="text-xs text-muted-foreground">
				Signature is verified after the download and before anything is written.
			</p>
			<SheetFooter className="px-0">
				<Button variant="outline" onClick={() => useUpdateSheetStore.getState().close()}>
					Not now
				</Button>
				<Button onClick={() => void updater.install()}>Update now</Button>
			</SheetFooter>
		</div>
	);
}

/* ───── Packages tab ───── */

function PkgUpdatePanel() {
	const pkgs = usePkgsDerived();
	const updatePkgs = useUpdatePkgs();
	const [progress, setProgress] = useState<UpdateProgress | null>(null);
	const [failures, setFailures] = useState<UpdateFailure[]>([]);
	const [doneIds, setDoneIds] = useState<Set<string>>(new Set());

	// A fresh open (or a fresh batch of updates landing) clears the last run's
	// result rather than showing a stale done/failed state forever.
	useEffect(() => {
		setFailures([]);
		setDoneIds(new Set());
	}, [pkgs.updates.length]);

	if (pkgs.updates.length === 0) {
		return <p className="text-sm text-muted-foreground">Every installed package is up to date.</p>;
	}

	function runBatch() {
		setFailures([]);
		updatePkgs.mutate(
			{ rows: pkgs.updates, onProgress: setProgress },
			{
				onSuccess: (res) => {
					if (res.failed.length) setFailures(res.failed);
					setDoneIds((prev) => {
						const next = new Set(prev);
						for (const row of pkgs.updates) {
							if (!res.failed.some((f) => f.id === row.id)) next.add(row.id);
						}
						return next;
					});
				},
				onSettled: () => setProgress(null),
			}
		);
	}

	return (
		<div data-state="update-packages" className="space-y-4">
			<h3 className="font-display text-sm font-semibold">
				Package updates <span className="font-mono">{pkgs.updates.length}</span>
			</h3>
			<ul className="space-y-1.5">
				{pkgs.updates.map((row) => {
					const isCurrent = updatePkgs.isPending && progress?.current === row.name;
					const isDone = doneIds.has(row.id);
					const failure = failures.find((f) => f.id === row.id);
					return (
						<li
							key={row.id}
							aria-busy={isCurrent || undefined}
							className="flex items-center justify-between gap-3 rounded-md border border-border bg-background px-3 py-1.5"
						>
							<span className="min-w-0">
								<span className="block truncate font-mono text-sm">{row.name}</span>
								<span className="block font-mono text-[11px] text-muted-foreground">
									{row.version} → {row.latest}
								</span>
							</span>
							{isCurrent ? (
								<span className="ember-dots shrink-0" aria-hidden="true">
									<i />
									<i />
									<i />
								</span>
							) : failure ? (
								<span
									className="flex shrink-0 items-center gap-1 text-[11px]"
									style={{ color: 'var(--danger)' }}
								>
									<AlertTriangle className="h-3 w-3" /> failed
								</span>
							) : isDone ? (
								<span
									className="flex shrink-0 items-center gap-1 text-[11px]"
									style={{ color: 'var(--live)' }}
								>
									<CheckCircle2 className="h-3 w-3" /> updated
								</span>
							) : null}
						</li>
					);
				})}
			</ul>
			{failures.length > 0 && (
				<p className="text-sm" style={{ color: 'var(--danger)' }}>
					{plural(failures.length, 'package')} failed —{' '}
					{failures.map((f) => `${f.name}: ${f.error}`).join(' · ')}
				</p>
			)}
			<p className="text-xs text-muted-foreground">
				Each package is verified and re-registered on its own; one failure never rolls back the
				others.
			</p>
			<SheetFooter className="px-0">
				<Button onClick={runBatch} disabled={updatePkgs.isPending}>
					{updatePkgs.isPending
						? `Updating ${progress ? `${progress.done}/${progress.total}` : '…'}`
						: `Update all (${pkgs.updates.length})`}
				</Button>
			</SheetFooter>
		</div>
	);
}
