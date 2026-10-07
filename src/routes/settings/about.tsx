// Settings → About. The home for everything-version-related on the shell
// itself (the kernel + bundled chrome). Pkg versions live in /packages.
//
// Surface order:
//   1. Header strip — shell name, current version, last check time, [Check now]
//   1b. Server updates (browser admins / the T0 operator only, WP-P9)
//   2. Available-update card (conditional) — vX → vY, release notes, [Update]
//      → [Restart now] once installed, [Defer 24h]
//   3. Changelog feed — last 20 releases, collapsible
//
// Update mutation routes through the existing `useUpdater` hook (Tauri's
// plugin-updater) so the signing + bundle verification stays identical to
// the legacy UpdaterBanner.

import { createFileRoute } from '@tanstack/react-router';
import { BellOff, Download, ExternalLink, Loader2, RefreshCw } from 'lucide-react';

import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';
import { Markdown } from '@/components/markdown';
import { cn } from '@/components/ui/utils';
import { useServerUpdate } from '@/lib/queries/server-update';
import { useShellStore } from '@/lib/shell/shell-store';
import { isTauri } from '@/lib/transport';
import {
	findReleaseByVersion,
	useGitHubReleases,
	type GitHubRelease,
} from '@/lib/updater/use-github-releases';
import { useShellVersion } from '@/lib/updater/use-shell-version';
import { useUpdater } from '@/lib/updater/use-updater';
import { useUpdaterSnooze } from '@/lib/updater/snooze';

import { ServerUpdatePanel } from './-components/server-update-panel';
import { SettingGroup } from './-components/setting-group';
import { SettingRow } from './-components/setting-row';
import { formatRelative, UpdateStatus } from './-components/update-status';

function AboutPage() {
	const updater = useUpdater({ autoPoll: false });
	const releases = useGitHubReleases();
	const shellVersion = useShellVersion();
	const snooze = useUpdaterSnooze();

	const currentVersion = updater.available?.currentVersion ?? shellVersion ?? '—';
	const isSnoozed = snooze.isSnoozed(updater.available?.version ?? null);
	const matchingRelease = findReleaseByVersion(releases.data, updater.available?.version ?? '');
	// The in-app updater is desktop-only; a browser session never checks.
	const desktop = isTauri();
	// WP-P9: a browser admin (T1) or the T0 operator may update the server.
	const serverUpdate = useServerUpdate();

	return (
		<div className="mx-auto w-full max-w-[720px] space-y-5 px-6 py-6">
			<SettingGroup title="Current build">
				<HeaderStrip
					desktop={desktop}
					currentVersion={currentVersion}
					lastCheckedAt={updater.lastCheckedAt}
					checking={updater.checking}
					onCheck={() => void updater.check()}
				/>
			</SettingGroup>

			{!desktop && serverUpdate.data && <ServerUpdatePanel />}

			<AutoUpdateSettings />

			{updater.available && (
				<UpdateCard
					availableVersion={updater.available.version}
					currentVersion={currentVersion}
					notes={matchingRelease?.body ?? updater.available.notes ?? ''}
					htmlUrl={matchingRelease?.htmlUrl}
					installing={updater.installing}
					installed={updater.installed}
					bytesDownloaded={updater.bytesDownloaded}
					totalBytes={updater.totalBytes}
					error={updater.error}
					snoozed={isSnoozed}
					onInstall={() => void updater.install()}
					onRestart={() => void updater.restart()}
					onSnooze={() => snooze.snooze(updater.available!.version)}
					onUnsnooze={() => snooze.clear()}
				/>
			)}

			<UpdateStatus
				desktop={desktop}
				available={!!updater.available}
				checking={updater.checking}
				lastCheckedAt={updater.lastCheckedAt}
				serverUpdates={!!serverUpdate.data}
			/>

			<ChangelogFeed
				releases={releases.data ?? null}
				loading={releases.isLoading}
				error={releases.error as Error | null}
				currentVersion={currentVersion}
			/>
		</div>
	);
}

/* ───── Auto-update settings ───── */

function AutoUpdateSettings() {
	const autoCheck = useShellStore((s) => s.updatesAutoCheck);
	const setAutoCheck = useShellStore((s) => s.setUpdatesAutoCheck);
	const autoInstallApp = useShellStore((s) => s.updatesAutoInstallApp);
	const setAutoInstallApp = useShellStore((s) => s.setUpdatesAutoInstallApp);
	const autoInstallPkgs = useShellStore((s) => s.updatesAutoInstallPkgs);
	const setAutoInstallPkgs = useShellStore((s) => s.setUpdatesAutoInstallPkgs);

	return (
		<SettingGroup title="Automatic updates">
			<SettingRow
				label="Check for updates automatically"
				desc="Checks Ikenga, your apps and your extensions on launch and every 6 hours. Turn off to only check manually with the button above."
			>
				<Switch checked={autoCheck} onCheckedChange={setAutoCheck} />
			</SettingRow>
			<SettingRow
				label="Install app and extension updates in the background"
				desc="Apps and extensions are sandboxed and reload in place — no restart. Recommended on."
			>
				<Switch
					checked={autoInstallPkgs}
					onCheckedChange={setAutoInstallPkgs}
					disabled={!autoCheck}
				/>
			</SettingRow>
			<SettingRow
				label="Install app updates automatically"
				desc="Off by default. When on, a new build downloads and installs in the background — Ikenga never restarts on its own, you press Restart when you're ready (deferring still works)."
			>
				<Switch
					checked={autoInstallApp}
					onCheckedChange={setAutoInstallApp}
					disabled={!autoCheck}
				/>
			</SettingRow>
		</SettingGroup>
	);
}

/* ───── Header strip ───── */

function HeaderStrip({
	desktop,
	currentVersion,
	lastCheckedAt,
	checking,
	onCheck,
}: {
	desktop: boolean;
	currentVersion: string;
	lastCheckedAt: number | null;
	checking: boolean;
	onCheck: () => void;
}) {
	return (
		<div className="flex items-center gap-4 px-4 py-3.5">
			<div className="flex h-10 w-10 items-center justify-center rounded-md border border-border bg-muted/40 font-display text-base font-semibold text-primary">
				IK
			</div>
			<div className="min-w-0 flex-1">
				<div className="flex items-baseline gap-2">
					<span className="font-display text-base font-medium">Ikenga</span>
					<span className="rounded-sm border border-border bg-background px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground">
						v{currentVersion}
					</span>
				</div>
				{desktop && (
					<div className="font-mono text-[11px] text-muted-foreground/70">
						Last checked {formatRelative(lastCheckedAt)}
					</div>
				)}
			</div>
			{desktop && (
				<Button size="sm" variant="outline" disabled={checking} onClick={onCheck}>
					{checking ? (
						<Loader2 className="mr-1.5 size-3.5 animate-spin" />
					) : (
						<RefreshCw className="mr-1.5 size-3.5" />
					)}
					{checking ? 'Checking…' : 'Check now'}
				</Button>
			)}
		</div>
	);
}

/* ───── Available-update card ───── */

function UpdateCard({
	availableVersion,
	currentVersion,
	notes,
	htmlUrl,
	installing,
	installed,
	bytesDownloaded,
	totalBytes,
	error,
	snoozed,
	onInstall,
	onRestart,
	onSnooze,
	onUnsnooze,
}: {
	availableVersion: string;
	currentVersion: string;
	notes: string;
	htmlUrl?: string;
	installing: boolean;
	bytesDownloaded: number;
	totalBytes: number | null;
	installed: boolean;
	error: string | null;
	snoozed: boolean;
	onInstall: () => void;
	onRestart: () => void;
	onSnooze: () => void;
	onUnsnooze: () => void;
}) {
	const pct =
		totalBytes && totalBytes > 0
			? Math.min(100, Math.round((bytesDownloaded / totalBytes) * 100))
			: null;
	return (
		<section
			className={cn(
				'overflow-hidden rounded-lg border bg-card',
				snoozed
					? 'border-[var(--border-soft)] opacity-80'
					: 'border-amber-500/40 shadow-[0_0_0_1px_rgba(217,119,6,0.15)]'
			)}
		>
			<header className="flex items-center gap-3 border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
				<Download className={cn('size-4', snoozed ? 'text-muted-foreground' : 'text-amber-500')} />
				<div className="flex items-baseline gap-2">
					<span className="font-display text-sm font-semibold">Update available</span>
					<span className="font-mono text-[11px] text-muted-foreground">
						v{currentVersion} → v{availableVersion}
					</span>
					{snoozed && (
						<span className="rounded-sm border border-border bg-background px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
							snoozed 24h
						</span>
					)}
				</div>
				{htmlUrl && (
					<a
						href={htmlUrl}
						target="_blank"
						rel="noreferrer"
						className="ml-auto inline-flex items-center gap-1 font-mono text-[11px] text-muted-foreground hover:text-foreground"
					>
						GitHub release <ExternalLink className="size-3" />
					</a>
				)}
			</header>

			<div className="space-y-4 p-4">
				{notes ? (
					<div className="prose-sm max-w-none rounded-md border border-border bg-background p-3">
						<Markdown content={notes} />
					</div>
				) : (
					<p className="text-sm text-muted-foreground">No release notes published.</p>
				)}

				{error && (
					<div className="rounded-sm border border-red-500/40 bg-red-500/10 px-3 py-2 text-sm text-red-500">
						Update failed: {error}
					</div>
				)}

				{installing && (
					<div className="space-y-1.5">
						<div className="flex items-center justify-between font-mono text-[11px] text-muted-foreground">
							<span>Downloading…</span>
							<span>
								{pct !== null ? `${pct}%` : `${(bytesDownloaded / 1024 / 1024).toFixed(1)} MB`}
							</span>
						</div>
						<div className="h-1.5 overflow-hidden rounded-full bg-muted">
							<div
								className="h-full bg-amber-500 transition-all"
								style={{ width: pct !== null ? `${pct}%` : '50%' }}
							/>
						</div>
					</div>
				)}

				{installed && (
					<div className="rounded-sm border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400">
						v{availableVersion} is installed. Restart Ikenga to finish updating.
					</div>
				)}

				<div className="flex items-center justify-end gap-2">
					{installed ? (
						<Button
							size="sm"
							className="bg-emerald-600 text-emerald-50 hover:bg-emerald-600/90"
							onClick={onRestart}
						>
							<RefreshCw className="mr-1.5 size-3.5" />
							Restart now
						</Button>
					) : (
						<>
							{snoozed ? (
								<Button size="sm" variant="ghost" onClick={onUnsnooze}>
									<BellOff className="mr-1.5 size-3.5" />
									Unsnooze
								</Button>
							) : (
								<Button size="sm" variant="ghost" onClick={onSnooze} disabled={installing}>
									<BellOff className="mr-1.5 size-3.5" />
									Defer 24h
								</Button>
							)}
							<Button
								size="sm"
								className="bg-amber-500 text-amber-950 hover:bg-amber-500/90"
								onClick={onInstall}
								disabled={installing}
							>
								<Download className="mr-1.5 size-3.5" />
								{installing ? 'Installing…' : 'Update & restart'}
							</Button>
						</>
					)}
				</div>
			</div>
		</section>
	);
}

/* ───── Changelog feed ───── */

function ChangelogFeed({
	releases,
	loading,
	error,
	currentVersion,
}: {
	releases: GitHubRelease[] | null;
	loading: boolean;
	error: Error | null;
	currentVersion: string;
}) {
	return (
		<SettingGroup title="Changelog">
			{loading ? (
				<div className="flex items-center gap-2 px-4 py-3 text-sm text-muted-foreground">
					<Loader2 className="size-3.5 animate-spin" />
					Loading releases…
				</div>
			) : error ? (
				<div className="px-4 py-3 text-sm text-red-500">
					Could not load releases: {error.message}
				</div>
			) : releases && releases.length > 0 ? (
				releases.map((r) => (
					<ChangelogEntry key={r.tagName} release={r} currentVersion={currentVersion} />
				))
			) : (
				<div className="px-4 py-3 text-sm text-muted-foreground">No releases yet.</div>
			)}
		</SettingGroup>
	);
}

function ChangelogEntry({
	release,
	currentVersion,
}: {
	release: GitHubRelease;
	currentVersion: string;
}) {
	const version = release.tagName.replace(/^v/, '');
	const isCurrent = version === currentVersion;
	return (
		<details className="group px-4 py-3 [&_summary::-webkit-details-marker]:hidden">
			<summary className="flex cursor-pointer items-center gap-3">
				<span className="font-mono text-sm font-medium text-foreground">{release.tagName}</span>
				<span className="font-mono text-[11px] text-muted-foreground">
					{formatDate(release.publishedAt)}
				</span>
				{release.prerelease && (
					<span className="rounded-sm border border-border bg-background px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
						pre-release
					</span>
				)}
				{isCurrent && (
					<span className="rounded-sm border border-emerald-500/40 bg-emerald-500/10 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wider text-emerald-500">
						current
					</span>
				)}
				<span className="ml-auto font-mono text-[11px] text-muted-foreground/70 group-open:hidden">
					expand
				</span>
				<span className="ml-auto font-mono text-[11px] text-muted-foreground/70 hidden group-open:inline">
					collapse
				</span>
			</summary>
			<div className="mt-3 rounded-md border border-border bg-background p-3 text-sm">
				{release.body ? (
					<Markdown content={release.body} />
				) : (
					<p className="text-muted-foreground">No release notes.</p>
				)}
			</div>
		</details>
	);
}

/* ───── Time helpers ───── */

function formatDate(iso: string): string {
	try {
		return new Date(iso).toISOString().slice(0, 10);
	} catch {
		return iso;
	}
}

export const Route = createFileRoute('/settings/about')({
	component: AboutPage,
});
