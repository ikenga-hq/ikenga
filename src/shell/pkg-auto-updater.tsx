// Headless background pkg auto-updater. Mounted once in the workspace
// alongside <UpdaterBanner /> (which owns the app-binary side) — together the
// two sources of the WP-41 D-07 `update-flow` surface
// (`designs/system-flows.html?state=update-flow`, package-updates variant).
// Mounting usePkgsDerived here is what performs the boot-time + 6h registry
// check for pkg updates — the activity-bar badge and /packages surface
// subscribe to the same queries.
//
// When `updates.autoCheck` AND `updates.autoInstallPkgs` are both on, any
// outdated pkg is updated in place silently — pkgs are sandboxed and
// hot-reload via the kernel's `pkg-reloaded` event, so there's no relaunch and
// the surprise cost is low (unlike an app-binary update). A small dismissible
// strip confirms what was updated. When auto-install is off, this banner
// offers "Update all" instead, opening the shared `<UpdateSheet>` at its
// Packages tab (`update-sheet.tsx`) — the same batch list + progress either
// path ends up rendering; only who kicks it off differs.

import { useEffect, useRef, useState } from 'react';
import { AlertTriangle, CheckCircle2, Package } from 'lucide-react';
import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import { usePkgsDerived } from '@/lib/pkgs/use-derived';
import { useUpdateSheetStore } from '@/lib/updater/sheet-store';
import { useUpdatePkgs, type UpdateFailure, type UpdateProgress } from '@/lib/pkgs/use-update-pkgs';
import { useShellStore } from '@/lib/shell/shell-store';

function plural(n: number, one: string, many = `${one}s`): string {
	return `${n} ${n === 1 ? one : many}`;
}

export function PkgAutoUpdater() {
	const autoCheck = useShellStore((s) => s.updatesAutoCheck);
	const autoInstallPkgs = useShellStore((s) => s.updatesAutoInstallPkgs);
	const d = usePkgsDerived();
	const updatePkgs = useUpdatePkgs();
	const openSheet = useUpdateSheetStore((s) => s.openSheet);
	// id@latest of every update we've already kicked off this session, so a
	// query refetch (or the post-update invalidation) can't re-trigger the
	// same upgrade. A genuinely newer release later still fires (different key).
	const attempted = useRef<Set<string>>(new Set());
	const [progress, setProgress] = useState<UpdateProgress | null>(null);
	const [doneCount, setDoneCount] = useState<number | null>(null);
	const [failures, setFailures] = useState<UpdateFailure[]>([]);

	useEffect(() => {
		if (!autoCheck || !autoInstallPkgs) return;
		if (updatePkgs.isPending || !d.updates.length) return;
		const fresh = d.updates.filter((r) => !attempted.current.has(`${r.id}@${r.latest}`));
		if (!fresh.length) return;
		for (const r of fresh) attempted.current.add(`${r.id}@${r.latest}`);
		updatePkgs.mutate(
			{ rows: fresh, onProgress: setProgress },
			{
				onSuccess: (res) => {
					if (res.updated > 0) setDoneCount(res.updated);
					if (res.failed.length) setFailures(res.failed);
				},
				onSettled: () => setProgress(null),
			}
		);
	}, [autoCheck, autoInstallPkgs, d.updates, updatePkgs]);

	if (progress) {
		return (
			<Banner data-state="update-packages" tone="info" icon={<Package />}>
				<span className="inline-flex items-center gap-2">
					<span className="ember-dots" aria-hidden="true">
						<i />
						<i />
						<i />
					</span>
					Updating <span className="font-medium">{progress.current || 'packages'}</span> (
					{progress.done}/{progress.total})…
				</span>
			</Banner>
		);
	}

	// Failures outrank the success strip — they used to be swallowed entirely,
	// which read as "the update ran and nothing happened".
	if (failures.length > 0) {
		return (
			<Banner data-state="update-packages" tone="danger" icon={<AlertTriangle />} onDismiss={() => setFailures([])}>
				{doneCount ? (
					<span>
						Updated <span className="font-medium">{doneCount}</span>,{' '}
					</span>
				) : null}
				<span className="font-medium">
					{failures.length} package{failures.length === 1 ? '' : 's'} failed to update
				</span>
				<span className="text-muted-foreground">
					{' '}
					— {failures.map((f) => `${f.name}: ${f.error}`).join(' · ')}
				</span>
			</Banner>
		);
	}

	if (doneCount && doneCount > 0) {
		return (
			<Banner data-state="update-packages" tone="success" icon={<CheckCircle2 />} onDismiss={() => setDoneCount(null)}>
				Updated <span className="font-medium">{doneCount}</span> package
				{doneCount === 1 ? '' : 's'}.
			</Banner>
		);
	}

	// Auto-install is on — the effect above handles it silently; nothing to
	// offer here while there's no progress/result to report.
	if (autoCheck && autoInstallPkgs) return null;

	// Manual path: updates exist but auto-install is off. Offer the same
	// batch flow the mockup's pkg banner does ("N package updates available
	// · Update all (N)"), opening the shared sheet's Packages tab rather than
	// running the batch inline — the badge + /packages strip still work too.
	if (!d.updates.length) return null;
	const names = d.updates
		.slice(0, 2)
		.map((r) => r.name)
		.join(', ');
	return (
		<Banner
			data-state="update-packages"
			tone="warning"
			icon={<Package />}
			actions={
				<Button size="sm" onClick={() => openSheet('pkgs')}>
					Update all ({d.updates.length})
				</Button>
			}
		>
			<span className="font-medium">{plural(d.updates.length, 'package update')}</span>
			<span className="text-muted-foreground"> available</span>
			{names && <span className="text-muted-foreground"> · {names}</span>}
		</Banner>
	);
}
