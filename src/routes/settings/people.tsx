import { createFileRoute } from '@tanstack/react-router';
import { Users } from 'lucide-react';

// D-03 §7: summary card only — the People, devices and access surface (D-05)
// arrives with Phase 5b. The card renders static placeholders until then.

function PeoplePage() {
	return (
		<div className="mx-auto w-full max-w-[720px] space-y-6 px-6 py-6">
			<header className="space-y-1">
				<h2
					className="text-2xl font-semibold tracking-tight"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					People &amp; devices
				</h2>
				<p className="max-w-prose text-sm leading-relaxed text-muted-foreground">
					Who this workspace is shared with, and on which devices.
				</p>
			</header>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Summary
					</h3>
				</header>
				<div className="grid grid-cols-3 gap-4 px-4 py-4 text-sm">
					<div className="space-y-1">
						<div className="text-2xl font-semibold text-foreground">1</div>
						<div className="text-xs text-muted-foreground">Member</div>
					</div>
					<div className="space-y-1">
						<div className="text-2xl font-semibold text-foreground">1</div>
						<div className="text-xs text-muted-foreground">Device</div>
					</div>
					<div className="space-y-1">
						<div className="text-2xl font-semibold text-foreground">0</div>
						<div className="text-xs text-muted-foreground">Pending invites</div>
					</div>
				</div>
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<div className="flex items-center gap-3 px-4 py-5 text-sm text-muted-foreground">
					<Users className="h-4 w-4 shrink-0" />
					<span>
						Sharing, devices and access arrive with the People surface.
					</span>
				</div>
			</section>
		</div>
	);
}

export const Route = createFileRoute('/settings/people')({
	component: PeoplePage,
});
