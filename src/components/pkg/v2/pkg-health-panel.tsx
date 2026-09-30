// Settings → Packages → Health (install-integrity check + cleanup).
//
// Surfaces broken / orphaned package install records the kernel skips at boot:
// `pkg_installed` rows whose manifest is missing / unreadable / unparseable /
// api-incompatible, plus orphaned child `pkg_*` rows. Offers per-row removal
// and "Remove all". The kernel is the only writer of `pkg_installed`, so every
// action routes through it (pkg_health_* commands). Never auto-deletes — a
// "missing" path can be a dev pkg you haven't checked out yet.
//
// Sibling to pkg-violations-audit.tsx (the cross-pkg permission audit); both
// live under the pkg diagnostics surfaces.

import { PackageX, RefreshCw, Trash2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { FeedbackState } from '@/components/ui/feedback-state';
import {
	pkgHealthRemove,
	pkgHealthRemoveAll,
	pkgHealthScan,
	type PkgHealthIssue,
	type PkgHealthIssueKind,
} from '@/lib/tauri-cmd';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';

/** Short label for the issue-kind badge. */
function kindLabel(kind: PkgHealthIssueKind): string {
	switch (kind.kind) {
		case 'manifest_missing':
			return 'missing manifest';
		case 'manifest_unreadable':
			return 'unreadable';
		case 'manifest_unparseable':
			return 'invalid manifest';
		case 'api_incompatible':
			return `api ${kind.ikenga_api}`;
		case 'orphan_row':
			return `orphan: ${kind.table}`;
		case 'pkgs_dir_unloadable':
			return 'failed to load';
		case 'register_failed':
			return 'not registered';
	}
}

/** Orphan rows are lower-severity (no parent to break); broken installs are
 *  the loud case. */
function kindTone(kind: PkgHealthIssueKind): 'broken' | 'orphan' {
	return kind.kind === 'orphan_row' ? 'orphan' : 'broken';
}

const TONE_CLASSES: Record<string, string> = {
	broken: 'border-destructive/40 bg-destructive/10 text-destructive',
	orphan: 'border-border bg-muted/40 text-muted-foreground',
};

function KindBadge({ kind }: { kind: PkgHealthIssueKind }) {
	const tone = kindTone(kind);
	return (
		<span
			className={`inline-block rounded-sm border px-1.5 py-0.5 font-mono text-[10px] uppercase ${TONE_CLASSES[tone]}`}
		>
			{kindLabel(kind)}
		</span>
	);
}

export function PkgHealthPanel() {
	const qc = useQueryClient();

	const healthQuery = useQuery({
		queryKey: ['pkg', 'health'],
		queryFn: () => pkgHealthScan(),
		refetchOnWindowFocus: true,
	});

	const removeMut = useMutation({
		mutationFn: (pkgId: string) => pkgHealthRemove(pkgId),
		onSuccess: () => qc.invalidateQueries({ queryKey: ['pkg'] }),
	});

	const removeAllMut = useMutation({
		mutationFn: () => pkgHealthRemoveAll(),
		onSuccess: () => qc.invalidateQueries({ queryKey: ['pkg'] }),
	});

	const rows: PkgHealthIssue[] = healthQuery.data ?? [];
	const busy = removeMut.isPending || removeAllMut.isPending;

	return (
		<div className="space-y-6 p-6">
			<div className="flex items-center justify-between">
				<div>
					<h2 className="font-display text-base font-semibold">
						Package health
						{rows.length > 0 && (
							<span className="ml-2 rounded-sm border border-destructive/40 bg-destructive/10 px-1.5 py-0.5 font-mono text-[11px] text-destructive">
								{rows.length}
							</span>
						)}
					</h2>
					<p className="mt-0.5 text-[11.5px] text-muted-foreground">
						Broken or orphaned install records — rows whose manifest is missing, unreadable,
						invalid, or api-incompatible, plus orphaned child rows the kernel skips at boot.
						Removing a record deletes its database rows; a pkgs-folder entry that failed to load
						is moved to a recoverable backup folder, never deleted.
					</p>
				</div>
				<div className="flex shrink-0 items-center gap-1.5">
					{rows.length > 0 && (
						<Button
							size="sm"
							variant="ghost"
							className="text-destructive hover:bg-destructive/10 hover:text-destructive"
							disabled={busy}
							onClick={() => removeAllMut.mutate()}
						>
							<Trash2 className="mr-1 h-3.5 w-3.5" />
							Remove all
						</Button>
					)}
					<Button
						size="sm"
						variant="ghost"
						disabled={healthQuery.isFetching}
						onClick={() => healthQuery.refetch()}
						aria-label="Re-scan package health"
					>
						<RefreshCw className={`h-3.5 w-3.5 ${healthQuery.isFetching ? 'animate-spin' : ''}`} />
					</Button>
				</div>
			</div>

			{healthQuery.isLoading && <FeedbackState variant="loading" heading="Scanning installs…" />}
			{healthQuery.error && (
				<FeedbackState
					variant="error"
					heading="Health scan failed"
					body={(healthQuery.error as Error).message}
				/>
			)}
			{!healthQuery.isLoading && rows.length === 0 && (
				<FeedbackState
					variant="empty"
					heading="All installs healthy."
					body="No broken or orphaned package records."
				/>
			)}

			{rows.length > 0 && (
				<div className="overflow-hidden rounded-sm border border-border">
					<table className="w-full text-xs">
						<thead>
							<tr className="border-b border-border bg-muted/40">
								<th className="px-3 py-2 text-left font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
									pkg / row
								</th>
								<th className="px-3 py-2 text-left font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
									issue
								</th>
								<th className="px-3 py-2 text-left font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
									detail
								</th>
								<th className="px-3 py-2 text-right font-mono text-[10px] uppercase tracking-wider text-muted-foreground/70">
									action
								</th>
							</tr>
						</thead>
						<tbody className="divide-y divide-border">
							{rows.map((r) => (
								<tr key={`${r.id}:${r.issue.kind}`} className="bg-background hover:bg-muted/20">
									<td className="max-w-[180px] px-3 py-2">
										<div className="truncate font-mono text-[11px] text-foreground">{r.id}</div>
										{r.install_path && (
											<div className="truncate font-mono text-[10px] text-muted-foreground/70">
												{r.install_path}
											</div>
										)}
										{!r.enabled && (
											<span className="font-mono text-[10px] text-muted-foreground/60">
												disabled
											</span>
										)}
									</td>
									<td className="px-3 py-2">
										<KindBadge kind={r.issue} />
									</td>
									<td className="max-w-[280px] truncate px-3 py-2 font-mono text-[11px] text-muted-foreground">
										{r.detail}
									</td>
									<td className="px-3 py-2 text-right">
										<button
											type="button"
											disabled={busy}
											onClick={() => removeMut.mutate(r.id)}
											className="inline-flex items-center gap-1 rounded-sm border border-border bg-background px-2 py-0.5 font-mono text-[11px] text-muted-foreground hover:border-destructive/40 hover:bg-destructive/10 hover:text-destructive disabled:opacity-50"
										>
											<PackageX className="h-3 w-3" />
											Remove
										</button>
									</td>
								</tr>
							))}
						</tbody>
					</table>
					<div className="border-t border-border bg-muted/20 px-3 py-2 font-mono text-[10px] text-muted-foreground/70">
						{rows.length} unhealthy record{rows.length === 1 ? '' : 's'}. Removal deletes the
						`pkg_installed` row + its child `pkg_*` rows (or the orphan row) — files on disk are
						never touched.
					</div>
				</div>
			)}
		</div>
	);
}
