import { Link, createFileRoute } from '@tanstack/react-router';
import { DatabaseZap, ShieldAlert, Stethoscope } from 'lucide-react';

import { Button } from '@/components/ui/button';

import { BackupSectionBody } from './-components/backup-body';
import { ClearDataSectionBody } from './-components/clear-data';
import { ScreenshotDirSectionBody } from './-components/screenshot-dir';

function StoragePage() {
	return (
		<div className="mx-auto w-full max-w-[720px] space-y-6 px-6 py-6">
			<header className="space-y-1">
				<h2
					className="text-2xl font-semibold tracking-tight"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					Storage &amp; backup
				</h2>
				<p className="max-w-prose text-sm leading-relaxed text-muted-foreground">
					Where Ikenga reads from and writes to on disk — screenshot destination, the local SQLite
					and browser caches, backups, and the danger zone.
				</p>
			</header>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						File roots
					</h3>
				</header>
				<div className="px-4 py-3 text-xs leading-relaxed text-muted-foreground">
					Extra file roots moved to{' '}
					<Link to="/settings/projects" className="text-primary underline-offset-2 hover:underline">
						Projects
					</Link>
					, where each project owns its own roots.
				</div>
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Screenshots
					</h3>
				</header>
				<ScreenshotDirSectionBody />
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Backup &amp; restore
					</h3>
				</header>
				<div className="px-4 py-3">
					<BackupSectionBody />
				</div>
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Local data
					</h3>
				</header>
				<ClearDataSectionBody />
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Ngwa health
					</h3>
				</header>
				<div className="divide-y divide-border">
					<HealthRedirectRow
						to="/ngwa/health"
						search={{ section: 'violations' }}
						Icon={ShieldAlert}
						label="Pkg violations"
					/>
					<HealthRedirectRow
						to="/ngwa/health"
						search={{ section: 'violations' }}
						Icon={Stethoscope}
						label="Pkg health"
					/>
					<HealthRedirectRow
						to="/ngwa/health"
						search={{ section: 'data' }}
						Icon={DatabaseZap}
						label="Data health"
					/>
				</div>
			</section>
		</div>
	);
}

function HealthRedirectRow({
	to,
	search,
	Icon,
	label,
}: {
	to: '/ngwa/health';
	search: { section: 'violations' | 'data' };
	Icon: typeof ShieldAlert;
	label: string;
}) {
	return (
		<div className="flex items-center justify-between gap-4 px-4 py-2.5">
			<div className="flex min-w-0 items-center gap-2 text-sm text-foreground">
				<Icon className="h-4 w-4 shrink-0 text-muted-foreground" />
				<span className="truncate">{label}</span>
			</div>
			<Button asChild variant="outline" size="sm">
				<Link to={to} search={search}>
					Ngwa → Health
				</Link>
			</Button>
		</div>
	);
}

export const Route = createFileRoute('/settings/storage')({
	component: StoragePage,
});
