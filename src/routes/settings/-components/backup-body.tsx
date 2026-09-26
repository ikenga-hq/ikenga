import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useState } from 'react';
import {
	save as saveDialog,
	confirm as confirmDialog,
} from '@/lib/transport/dialog-shim';
import {
	Download,
	Upload,
	Trash2,
	RefreshCw,
	AlertTriangle,
	CheckCircle2,
	Lock,
} from 'lucide-react';

import {
	backupExport,
	backupList,
	backupDelete,
	type BackupSummary,
	type ImportResult,
	type PathMode,
} from '@/lib/tauri-cmd';
import { Button } from '@/components/ui/button';
import { Card } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { RestoreWizard } from '@/shell/backup/restore-wizard';

const BACKUPS_QUERY_KEY = ['settings', 'backup', 'list'] as const;

export function BackupSectionBody() {
	const qc = useQueryClient();
	const backups = useQuery({
		queryKey: BACKUPS_QUERY_KEY,
		queryFn: () => backupList(),
	});

	const [busy, setBusy] = useState<null | 'export'>(null);
	const [error, setError] = useState<string | null>(null);
	const [success, setSuccess] = useState<string | null>(null);
	const [exportDialog, setExportDialog] = useState<{
		dest: string;
		includeSecrets: boolean;
		passphrase: string;
		confirmPassphrase: string;
		pathMode: PathMode;
	} | null>(null);
	const [restoreOpen, setRestoreOpen] = useState(false);

	const refreshList = () => qc.invalidateQueries({ queryKey: BACKUPS_QUERY_KEY });

	async function onPickExportDest() {
		setError(null);
		setSuccess(null);
		const stamp = new Date().toISOString().replace(/[:.]/g, '-');
		const dest = await saveDialog({
			defaultPath: `ikenga-backup-${stamp}.ikbak`,
			filters: [{ name: 'Ikenga backup', extensions: ['ikbak'] }],
		});
		if (!dest) return;
		setExportDialog({
			dest,
			includeSecrets: true,
			passphrase: '',
			confirmPassphrase: '',
			pathMode: 'raw',
		});
	}

	async function onConfirmExport() {
		if (!exportDialog) return;
		const { dest, includeSecrets, passphrase, confirmPassphrase, pathMode } = exportDialog;
		if (pathMode === 'bundled') {
			setError('Bundled path mode is not yet implemented (phase 4).');
			return;
		}
		if (includeSecrets) {
			if (!passphrase) {
				setError("Enter a passphrase or uncheck 'Include secrets'.");
				return;
			}
			if (passphrase !== confirmPassphrase) {
				setError("Passphrases don't match.");
				return;
			}
		}
		setBusy('export');
		setError(null);
		try {
			const res = await backupExport(dest, {
				includeSecrets,
				passphrase: includeSecrets ? passphrase : undefined,
				pathMode,
			});
			setExportDialog(null);
			const warnSuffix =
				res.path_warnings_count > 0
					? `, ${res.path_warnings_count} path${
							res.path_warnings_count === 1 ? '' : 's'
						} outside $HOME`
					: '';
			setSuccess(
				`Exported ${formatBytes(res.size_bytes)} â†’ ${shortPath(res.path)} ` +
					`(${res.pkg_count} pkg${res.pkg_count === 1 ? '' : 's'}, ` +
					`${res.secrets_count} secret${res.secrets_count === 1 ? '' : 's'}` +
					warnSuffix +
					`)`
			);
			refreshList();
		} catch (e) {
			setError(`Export failed: ${String(e)}`);
		} finally {
			setBusy(null);
		}
	}

	function onOpenRestoreWizard() {
		setError(null);
		setSuccess(null);
		setRestoreOpen(true);
	}

	function onRestoreStaged(res: ImportResult) {
		setSuccess(
			res.secrets_staged
				? 'Restore staged · secrets included. Restart to apply.'
				: 'Restore staged. Restart to apply.'
		);
	}

	async function onDelete(path: string) {
		const ok = await confirmDialog(`Delete this backup?\n${shortPath(path)}`, {
			title: 'Delete backup',
			kind: 'warning',
		});
		if (!ok) return;
		try {
			await backupDelete(path);
			refreshList();
		} catch (e) {
			setError(`Delete failed: ${String(e)}`);
		}
	}

	return (
		<div className="space-y-4">

			{error && (
				<Alert variant="destructive">
					<AlertTriangle className="h-4 w-4" />
					<AlertTitle>Error</AlertTitle>
					<AlertDescription>{error}</AlertDescription>
				</Alert>
			)}
			{success && (
				<Alert>
					<CheckCircle2 className="h-4 w-4" />
					<AlertTitle>Done</AlertTitle>
					<AlertDescription>{success}</AlertDescription>
				</Alert>
			)}

			<Card className="p-5">
				<div className="flex items-start justify-between gap-4">
					<div>
						<h2 className="text-base font-medium">Export now</h2>
						<p className="mt-1 text-sm text-muted-foreground">
							You'll be asked for a passphrase to protect any vault secrets in the bundle.
						</p>
					</div>
					<Button onClick={onPickExportDest} disabled={busy !== null}>
						<Download className="mr-2 h-4 w-4" />
						{busy === 'export' ? 'Exportingâ€¦' : 'Export'}
					</Button>
				</div>
			</Card>

			<Card className="p-5">
				<div className="flex items-start justify-between gap-4">
					<div>
						<h2 className="text-base font-medium">Restore from file</h2>
						<p className="mt-1 text-sm text-muted-foreground">
							Pick a <code>.ikbak</code> file. The wizard walks through what will be replaced
							before anything is applied.
						</p>
					</div>
					<Button variant="outline" onClick={onOpenRestoreWizard} disabled={busy !== null}>
						<Upload className="mr-2 h-4 w-4" />
						Restore
					</Button>
				</div>
			</Card>

			<Card className="p-5">
				<div className="mb-3 flex items-center justify-between">
					<h2 className="text-base font-medium">Local backups</h2>
					<Button
						variant="ghost"
						size="sm"
						onClick={() => refreshList()}
						disabled={backups.isFetching}
					>
						<RefreshCw className="mr-2 h-3.5 w-3.5" /> Refresh
					</Button>
				</div>
				{backups.isLoading ? (
					<p className="text-sm text-muted-foreground">Loadingâ€¦</p>
				) : backups.data && backups.data.length > 0 ? (
					<ul className="divide-y divide-border">
						{backups.data.map((b) => (
							<BackupRow key={b.path} b={b} onDelete={() => onDelete(b.path)} />
						))}
					</ul>
				) : (
					<p className="text-sm text-muted-foreground">
						No local backups yet. Auto-backup arrives in phase 4.
					</p>
				)}
			</Card>

			{/* Export dialog with passphrase fields */}
			<Dialog
				open={!!exportDialog}
				onOpenChange={(open) => {
					if (!open) setExportDialog(null);
				}}
			>
				<DialogContent>
					<DialogHeader>
						<DialogTitle>Export options</DialogTitle>
						<DialogDescription>
							Choose what to include. Secrets are encrypted with age + your passphrase before they
							leave the process.
						</DialogDescription>
					</DialogHeader>
					{exportDialog && (
						<div className="space-y-4 text-sm">
							<KV k="Destination" v={shortPath(exportDialog.dest)} />

							<div className="space-y-2">
								<div className="text-xs font-medium text-muted-foreground">Path mode</div>
								{(['raw', 'tokenized', 'bundled'] as PathMode[]).map((m) => (
									<label key={m} className="flex items-start gap-2" aria-disabled={m === 'bundled'}>
										<input
											type="radio"
											name="path-mode"
											className="mt-1"
											checked={exportDialog.pathMode === m}
											disabled={m === 'bundled'}
											onChange={() => setExportDialog((d) => (d ? { ...d, pathMode: m } : d))}
										/>
										<span>
											<span className="font-medium">{m}</span>
											<span className="block text-xs text-muted-foreground">
												{describePathMode(m)}
											</span>
										</span>
									</label>
								))}
							</div>

							<label className="flex items-center gap-2">
								<input
									type="checkbox"
									checked={exportDialog.includeSecrets}
									onChange={(e) =>
										setExportDialog((d) =>
											d
												? {
														...d,
														includeSecrets: e.target.checked,
													}
												: d
										)
									}
								/>
								<span>Include vault secrets (Stronghold)</span>
							</label>
							{exportDialog.includeSecrets && (
								<>
									<div className="space-y-1">
										<label className="text-xs text-muted-foreground">Passphrase</label>
										<Input
											type="password"
											autoFocus
											value={exportDialog.passphrase}
											onChange={(e) =>
												setExportDialog((d) => (d ? { ...d, passphrase: e.target.value } : d))
											}
										/>
									</div>
									<div className="space-y-1">
										<label className="text-xs text-muted-foreground">Confirm passphrase</label>
										<Input
											type="password"
											value={exportDialog.confirmPassphrase}
											onChange={(e) =>
												setExportDialog((d) =>
													d ? { ...d, confirmPassphrase: e.target.value } : d
												)
											}
										/>
									</div>
									<Alert>
										<Lock className="h-4 w-4" />
										<AlertDescription>
											If you lose this passphrase, the secrets in this bundle are unrecoverable. The
											database and pkg list remain restorable without it.
										</AlertDescription>
									</Alert>
								</>
							)}
						</div>
					)}
					<DialogFooter>
						<Button variant="outline" onClick={() => setExportDialog(null)}>
							Cancel
						</Button>
						<Button onClick={onConfirmExport} disabled={busy !== null}>
							Export
						</Button>
					</DialogFooter>
				</DialogContent>
			</Dialog>

			<RestoreWizard open={restoreOpen} onOpenChange={setRestoreOpen} onStaged={onRestoreStaged} />
		</div>
	);
}

function BackupRow({ b, onDelete }: { b: BackupSummary; onDelete: () => void }) {
	return (
		<li className="flex items-center justify-between gap-4 py-3">
			<div className="min-w-0">
				<div className="truncate text-sm font-medium">{shortPath(b.path)}</div>
				<div className="text-xs text-muted-foreground">
					{b.created_at || '(unparseable manifest)'} Â· {formatBytes(b.size_bytes)} Â· schema v
					{b.schema_version} Â· {b.path_mode}
					{b.has_secrets ? ' Â· with secrets' : ''}
					{b.pkg_count > 0 ? ` Â· ${b.pkg_count} pkgs` : ''}
				</div>
			</div>
			<Button variant="ghost" size="sm" onClick={onDelete}>
				<Trash2 className="h-4 w-4" />
			</Button>
		</li>
	);
}

function KV({ k, v }: { k: string; v: string }) {
	return (
		<div className="flex items-baseline justify-between gap-4">
			<span className="text-muted-foreground">{k}</span>
			<span className="truncate font-mono text-xs">{v}</span>
		</div>
	);
}

function describePathMode(m: PathMode): string {
	switch (m) {
		case 'raw':
			return 'Absolute paths kept as-is. Same-machine recovery only.';
		case 'tokenized':
			return 'Rewrite $HOME/... â†’ ${IKENGA_HOME}/... and back on restore. Cross-machine portable.';
		case 'bundled':
			return 'Copy referenced files into the bundle. Not yet implemented.';
	}
}

function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
	return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function shortPath(p: string): string {
	const home = '/home/';
	const idx = p.indexOf(home);
	if (idx < 0) return p;
	const slash = p.indexOf('/', idx + home.length);
	return slash > 0 ? `~${p.slice(slash)}` : p;
}

