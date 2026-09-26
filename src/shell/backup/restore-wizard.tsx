// Restore wizard — WP-43 (D-07 `backup-restore`,
// plans/shell-ux-rearchitecture/drafts/design-spec-D-03-07.md §D-07;
// designs/system-flows.html?state=backup-restore).
//
// Pick a file → passphrase → schema check → what will be replaced → confirm
// → progress → done, all over the existing `backup_import` command
// (src-tauri/src/commands/backup.rs:369, dry_run toggles preview vs stage).
// Frontend-only: the one gap was secret NAMES without unlocking the vault,
// closed by the new `secrets_index_names` command (reads
// `secrets-index.json` directly — never touches the live Stronghold store,
// never returns a value).
//
// Replaces the "preview dialog + confirm" restore flow in
// `routes/settings/-components/backup-body.tsx` with the missing step design
// calls out: a screen that says plainly what is about to be replaced before
// the user commits.

import { useEffect, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { AlertTriangle, CheckCircle2, FileArchive, Lock } from 'lucide-react';

import { Button } from '@/components/ui/button';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Progress } from '@/components/ui/progress';
import { cn } from '@/components/ui/utils';
import { ErrorState } from '@/components/states';
import { usePkgsDerived } from '@/lib/pkgs/use-derived';
import {
	backupImport,
	backupList,
	secretsIndexNames,
	type BackupSummary,
	type ImportPreview,
	type ImportResult,
} from '@/lib/tauri-cmd';
import { open as openFileDialog } from '@/lib/transport/dialog-shim';
import { restartApp } from '@/lib/updater/updater';

const STEP_LABELS = [
	'Pick a file',
	'Passphrase',
	'Schema check',
	'What is replaced',
	'Restore',
	'Done',
] as const;

type Step = 0 | 1 | 2 | 3 | 4 | 5;

const RESTORE_STAGES = [
	'Writing the safety copy…',
	'Applying the schema…',
	'Restoring the database…',
	'Restoring the vault…',
	'Queueing packages…',
];

export interface RestoreWizardProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
	/** Fires once the restore is staged (before any restart) — e.g. so the
	 *  caller can refresh its local-backups list. Not "restore applied": the
	 *  staged bundle only takes effect on next launch. */
	onStaged?: (result: ImportResult) => void;
}

export function RestoreWizard({ open, onOpenChange, onStaged }: RestoreWizardProps) {
	const [step, setStep] = useState<Step>(0);
	const [srcPath, setSrcPath] = useState<string | null>(null);
	const [preview, setPreview] = useState<ImportPreview | null>(null);
	const [passphrase, setPassphrase] = useState('');
	const [skipSecrets, setSkipSecrets] = useState(false);
	const [pathsOpen, setPathsOpen] = useState(false);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [progressPct, setProgressPct] = useState(0);
	const [result, setResult] = useState<ImportResult | null>(null);
	const tickRef = useRef<number | null>(null);

	const backups = useQuery({
		queryKey: ['settings', 'backup', 'list'],
		queryFn: () => backupList(),
		enabled: open && step === 0,
	});

	const currentSecrets = useQuery({
		queryKey: ['settings', 'backup', 'secrets-index'],
		queryFn: () => secretsIndexNames(),
		enabled: open && step === 3,
	});

	const pkgs = usePkgsDerived();

	function stopTicker() {
		if (tickRef.current !== null) {
			window.clearInterval(tickRef.current);
			tickRef.current = null;
		}
	}

	// Reset on close so reopening always starts clean.
	useEffect(() => {
		if (open) return;
		stopTicker();
		setStep(0);
		setSrcPath(null);
		setPreview(null);
		setPassphrase('');
		setSkipSecrets(false);
		setPathsOpen(false);
		setBusy(false);
		setError(null);
		setProgressPct(0);
		setResult(null);
		// eslint-disable-next-line react-hooks/exhaustive-deps
	}, [open]);

	useEffect(() => stopTicker, []);

	async function loadPreview(path: string) {
		setBusy(true);
		setError(null);
		try {
			const res = (await backupImport(path, { dryRun: true })) as ImportPreview;
			setPreview(res);
			setSrcPath(path);
			setStep(res.manifest.has_secrets ? 1 : 2);
		} catch (e) {
			setError(e instanceof Error ? e.message : String(e));
		} finally {
			setBusy(false);
		}
	}

	async function pickFile() {
		setError(null);
		const picked = await openFileDialog({
			multiple: false,
			filters: [{ name: 'Ikenga backup', extensions: ['ikbak'] }],
		});
		if (!picked || typeof picked !== 'string') return;
		void loadPreview(picked);
	}

	function continuePassphrase() {
		if (passphrase && passphrase.length < 4) {
			setError('That passphrase is too short to be the one.');
			return;
		}
		setSkipSecrets(passphrase.length === 0);
		setError(null);
		setStep(2);
	}

	function skipSecretsStep() {
		setPassphrase('');
		setSkipSecrets(true);
		setError(null);
		setStep(2);
	}

	function confirmRestore() {
		if (!srcPath) return;
		setStep(4);
		setProgressPct(0);
		setError(null);
		stopTicker();
		tickRef.current = window.setInterval(() => {
			setProgressPct((p) => Math.min(94, p + 6));
		}, 220);

		backupImport(srcPath, {
			dryRun: false,
			passphrase: skipSecrets ? undefined : passphrase || undefined,
		})
			.then((res) => {
				stopTicker();
				setProgressPct(100);
				const r = res as ImportResult;
				setResult(r);
				onStaged?.(r);
				window.setTimeout(() => setStep(5), 300);
			})
			.catch((e) => {
				stopTicker();
				setError(e instanceof Error ? e.message : String(e));
				setStep(3);
			});
	}

	const schemaBlocked = preview?.schema_action.kind === 'newer_than_app';
	const stageIndex = Math.min(
		RESTORE_STAGES.length - 1,
		Math.floor((progressPct / 100) * RESTORE_STAGES.length)
	);

	return (
		<Dialog open={open} onOpenChange={onOpenChange}>
			<DialogContent className="max-w-xl">
				<DialogHeader>
					<DialogTitle>Restore from a backup</DialogTitle>
					<DialogDescription>
						Step {step + 1} of {STEP_LABELS.length} · {STEP_LABELS[step]}
					</DialogDescription>
				</DialogHeader>

				<WizardStepper step={step} />

				<div className="min-h-[220px]">
					{error && step !== 4 && (
						<ErrorState
							data-state="restore-wizard-error"
							heading="Something went wrong"
							body={error}
							fill={false}
							className="mb-3 min-h-0 p-3"
						/>
					)}

					{step === 0 && (
						<div data-state="backup-restore-pick" className="space-y-3">
							<p className="text-sm text-muted-foreground">
								A backup is one file: the SQLite database, the package list with versions and
								scopes, your settings, and — if you asked for it at export time — the Stronghold
								vault, encrypted separately.
							</p>
							{backups.isLoading ? (
								<p className="text-xs text-muted-foreground">Loading local backups…</p>
							) : (
								(backups.data ?? []).map((b: BackupSummary) => (
									<BackupPickRow
										key={b.path}
										b={b}
										busy={busy}
										onPick={() => void loadPreview(b.path)}
									/>
								))
							)}
							<Button variant="outline" className="h-11" disabled={busy} onClick={() => void pickFile()}>
								<FileArchive className="mr-2 h-4 w-4" />
								Choose a file…
							</Button>
						</div>
					)}

					{step === 1 && preview && (
						<div data-state="backup-restore-passphrase" className="space-y-3">
							<p className="text-xs font-medium text-muted-foreground">
								This bundle carries secrets
							</p>
							<label className="space-y-1 block">
								<span className="text-xs text-muted-foreground">
									Passphrase — leave it blank to skip the secrets
								</span>
								<Input
									type="password"
									autoFocus
									value={passphrase}
									onChange={(e) => setPassphrase(e.target.value)}
									placeholder="••••••••••"
									autoComplete="off"
								/>
							</label>
							<p className="text-xs text-muted-foreground">
								Skipping is a real option, not a failure. The database, your settings and the
								package list restore without it; only the vault entries stay behind, and you can
								re-enter those one at a time in Settings · Secrets.
							</p>
							<p className="flex items-start gap-1.5 text-xs text-muted-foreground">
								<Lock className="mt-0.5 h-3.5 w-3.5 shrink-0" />
								If the passphrase is lost the vault in this bundle is unrecoverable. Nothing else
								is.
							</p>
						</div>
					)}

					{step === 2 && preview && (
						<div data-state="backup-restore-schema-check" className="space-y-2 text-sm">
							<KV k="Source" v={basename(srcPath ?? '')} />
							<KV k="Created" v={preview.manifest.created_at} />
							<KV k="From host" v={preview.manifest.hostname} />
							<KV k="Size" v={formatBytes(preview.size_bytes)} />
							<KV k="Schema" v={describeSchemaAction(preview.schema_action)} />
							<KV k="Secrets" v={skipSecrets ? 'skipped' : 'yes · passphrase accepted'} />
							<KV k="Packages" v={`${preview.pkgs.length}`} />
							<KV
								k="Path mode"
								v={`${preview.manifest.path_mode}${
									preview.manifest.home_dir ? ` (from ${preview.manifest.home_dir})` : ''
								}`}
							/>
							{preview.manifest.path_warnings.length > 0 && (
								<div className="space-y-1">
									<button
										type="button"
										className="text-xs text-primary underline-offset-2 hover:underline"
										onClick={() => setPathsOpen((v) => !v)}
									>
										{preview.manifest.path_warnings.length} path
										{preview.manifest.path_warnings.length === 1 ? '' : 's'} outside $HOME, kept
										raw — {pathsOpen ? 'hide' : 'show'}
									</button>
									{pathsOpen && (
										<ul className="space-y-0.5 rounded border border-border p-2 font-mono text-[11px]">
											{preview.manifest.path_warnings.slice(0, 50).map((w, i) => (
												<li key={`${w.table}.${w.column}.${i}`}>
													<span className="text-muted-foreground">
														{w.table}.{w.column}:
													</span>{' '}
													{w.value}
												</li>
											))}
										</ul>
									)}
								</div>
							)}
							{schemaBlocked && (
								<p className="flex items-start gap-1.5 text-xs" style={{ color: 'var(--danger)' }}>
									<AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />A backup newer than this
									build is refused outright — upgrade first.
								</p>
							)}
						</div>
					)}

					{step === 3 && preview && (
						<StepWhatIsReplaced
							preview={preview}
							skipSecrets={skipSecrets}
							currentSecretNames={currentSecrets.data ?? []}
							currentPkgCount={pkgs.installed.length}
							removedPkgCount={
								pkgs.installed.filter((row) => !preview.pkgs.some((p) => p.id === row.id)).length
							}
						/>
					)}

					{step === 4 && (
						<div data-state="backup-restore-progress" className="space-y-3 py-2">
							<Progress value={progressPct} />
							<p className="text-sm text-muted-foreground" aria-live="polite">
								{RESTORE_STAGES[stageIndex]}
							</p>
						</div>
					)}

					{step === 5 && result && (
						<div data-state="backup-restore-done" className="space-y-2 text-sm">
							<p className="flex items-center gap-1.5 font-medium" style={{ color: 'var(--live)' }}>
								<CheckCircle2 className="h-4 w-4" />
								Restore staged
							</p>
							<KV k="Database" v="restored" />
							<KV
								k="Packages"
								v={`${preview?.pkgs.length ?? 0} queued to re-register on restart`}
							/>
							<KV k="Secrets" v={result.secrets_staged ? 'restored' : 'skipped'} />
							<p className="text-xs text-muted-foreground">
								{result.requires_restart
									? 'Quit and reopen Ikenga to apply it. The running session is unchanged until then.'
									: 'Applied.'}
							</p>
						</div>
					)}
				</div>

				<DialogFooter className="flex-row items-center justify-between sm:justify-between">
					<div>
						{step > 0 && step < 4 && (
							<Button
								variant="ghost"
								className="h-11"
								onClick={() =>
									setStep(
										(s) =>
											(s === 2 && !preview?.manifest.has_secrets
												? 0
												: Math.max(0, s - 1)) as Step
									)
								}
							>
								Back
							</Button>
						)}
					</div>
					<div className="flex items-center gap-2">
						{step === 1 && (
							<Button variant="outline" className="h-11" onClick={skipSecretsStep}>
								Skip the secrets
							</Button>
						)}
						{step === 1 && (
							<Button className="h-11" onClick={continuePassphrase}>
								Continue
							</Button>
						)}
						{step === 2 && (
							<Button className="h-11" disabled={schemaBlocked} onClick={() => setStep(3)}>
								Continue
							</Button>
						)}
						{step === 3 && (
							<Button variant="destructive" className="h-11" onClick={confirmRestore}>
								Restore and replace
							</Button>
						)}
						{step === 5 && (
							<>
								<Button variant="outline" className="h-11" onClick={() => onOpenChange(false)}>
									I&rsquo;ll restart later
								</Button>
								<Button className="h-11" onClick={() => void restartApp()}>
									Restart to finish
								</Button>
							</>
						)}
					</div>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

function WizardStepper({ step }: { step: Step }) {
	return (
		<ol className="flex items-center gap-1.5 py-1" aria-label="Restore steps">
			{STEP_LABELS.map((label, i) => (
				<li key={label} className="flex min-w-0 flex-1 items-center gap-1.5">
					<span
						className={cn(
							'flex h-5 w-5 shrink-0 items-center justify-center rounded-full font-mono text-[10px]',
							i < step
								? 'bg-primary text-primary-foreground'
								: i === step
									? 'border-2 border-primary text-primary'
									: 'border border-border text-muted-foreground'
						)}
						aria-current={i === step ? 'step' : undefined}
					>
						{i + 1}
					</span>
					{i < STEP_LABELS.length - 1 && (
						<span
							className="h-px flex-1"
							style={{ background: i < step ? 'var(--primary)' : 'var(--border-soft)' }}
							aria-hidden="true"
						/>
					)}
				</li>
			))}
		</ol>
	);
}

function BackupPickRow({
	b,
	busy,
	onPick,
}: {
	b: BackupSummary;
	busy: boolean;
	onPick: () => void;
}) {
	return (
		<div className="flex items-center justify-between gap-3 rounded-md border border-border p-2.5">
			<div className="min-w-0">
				<div className="truncate font-mono text-xs">{basename(b.path)}</div>
				<div className="text-[11px] text-muted-foreground">
					{formatBytes(b.size_bytes)} · schema v{b.schema_version}
					{b.has_secrets ? ' · has secrets' : ''}
				</div>
			</div>
			<Button size="sm" variant="outline" className="h-11 shrink-0" disabled={busy} onClick={onPick}>
				Use this one
			</Button>
		</div>
	);
}

function StepWhatIsReplaced({
	preview,
	skipSecrets,
	currentSecretNames,
	currentPkgCount,
	removedPkgCount,
}: {
	preview: ImportPreview;
	skipSecrets: boolean;
	currentSecretNames: string[];
	currentPkgCount: number;
	removedPkgCount: number;
}) {
	return (
		<div data-state="backup-restore-replaces" className="space-y-2.5 text-sm">
			<ReplaceRow
				tone="bad"
				label="Replaced"
				body="Shell database — panes, tabs, session rows, artifact recents, pkg kernel state"
			/>
			<ReplaceRow
				tone="bad"
				label="Replaced"
				body={
					<>
						Installed package list — {preview.pkgs.length} in the backup, {currentPkgCount}{' '}
						currently installed
						{removedPkgCount > 0 && (
							<>
								; <b>{removedPkgCount}</b> not in the backup and will be removed
							</>
						)}
					</>
				}
			/>
			<ReplaceRow
				tone={skipSecrets ? 'ok' : 'bad'}
				label={skipSecrets ? 'Kept' : 'Merged'}
				body={
					skipSecrets
						? `Stronghold vault — yours is untouched (${currentSecretNames.length} key${currentSecretNames.length === 1 ? '' : 's'} today), the bundle's entries are ignored`
						: `Stronghold vault — entries in the bundle overwrite yours by key name (${currentSecretNames.length} key${currentSecretNames.length === 1 ? '' : 's'} today)`
				}
			/>
			<ReplaceRow tone="ok" label="Untouched" body="Everything on disk outside the app: your projects, your .claude/ folders, your artifacts" />
			<ReplaceRow tone="ok" label="Untouched" body="Project settings inside each repo <project>/.ikenga/" />
			<p className="pt-1 text-xs text-muted-foreground">
				A safety copy of the current database is written first, so this is reversible until you next
				export.
			</p>
		</div>
	);
}

function ReplaceRow({
	tone,
	label,
	body,
}: {
	tone: 'bad' | 'ok';
	label: string;
	body: React.ReactNode;
}) {
	return (
		<div className="flex items-start gap-3">
			<span
				className="w-20 shrink-0 text-xs font-medium"
				style={{ color: tone === 'bad' ? 'var(--danger)' : 'var(--live)' }}
			>
				{label}
			</span>
			<span className="text-muted-foreground">{body}</span>
		</div>
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

function describeSchemaAction(a: ImportPreview['schema_action']): string {
	switch (a.kind) {
		case 'match':
			return 'match';
		case 'forward':
			return `migrate · backup is ${a.to - a.from} migration${a.to - a.from === 1 ? '' : 's'} behind, will be applied`;
		case 'newer_than_app':
			return `newer than app (v${a.backup} > v${a.app})`;
	}
}

function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
	return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function basename(p: string): string {
	const idx = p.lastIndexOf('/');
	return idx >= 0 ? p.slice(idx + 1) : p;
}
