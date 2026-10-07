// Ngwa Health Surface (WP-16 / WP-16a / locked D-02 frame-workbench-v4.html).
//
// D-02 layout: a 2-column grid of six panels (VIOLATIONS · SIDECARS / CRON ·
// DATA / TRUST · ENGINES) plus the audit line. Panels grow to fit their rows
// and the page scrolls as a whole — nothing is clipped. Each panel reads its
// own real source and renders independently of the (slow) Ngwa snapshot:
//   1. Violations — the install-integrity scan (`pkgHealthScan`) and
//      `pkg_permission_violations`, one row each with inline actions: Reinstall
//      from registry (when the registry lists the pkg), Remove… (confirmed; a
//      pkgs-dir folder is retired to a recoverable backup, never deleted),
//      Clear, Hand to Chi. Remove all covers everything the scan lists and
//      reports exactly what it removed and what is still there.
//   2. Sidecars  — `pkgKernelStatus().registries.sidecar_supervisor`, Restart.
//   3. Cron      — agent-ops jobs (`agentOpsListJobs`, Run now, Logs) and pkg
//      manifest `cron[]` (`registries.cron`, no run history) — DEC-33.
//   4. Data      — DB file sizes (DEC-32), soft-FK orphan scan, newest backup.
//   5. Trust     — the unsigned count from the snapshot (gate §5), Review.
//   6. Engines   — the shared installed-engine signal + `detectAgent` probes.
// Anything not measured reads "—". Every failure renders as an error, never as
// an empty list or a zero. Destructive actions confirm first (DEC-30).

import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import {
	AlertTriangle,
	Bot,
	CheckCircle,
	Clock,
	Database,
	Play,
	RefreshCw,
	Shield,
} from 'lucide-react';
import { useMutation, useQueries, useQuery, useQueryClient } from '@tanstack/react-query';
import type { AgentOpsListJobsResult, AgentOpsRawJob, AgentOpsRunNowResult, NgwaItem, NgwaSnapshot } from '@ikenga/contract';
import {
	agentOpsListJobs,
	agentOpsRunNow,
	agentOpsTailRun,
	backupList,
	dataHealthDbSize,
	dataHealthScan,
	detectAgent,
	isUnregisteredPkgIssue,
	pkgHealthRemove,
	pkgHealthRemoveAll,
	pkgHealthScan,
	pkgKernelStatus,
	pkgPermissionViolationsClear,
	pkgPermissionViolationsList,
	pkgSupervisorRestart,
	type AgentOpsTailRunResult,
	type DbFileSizes,
	type EngineId,
	type PkgHealthIssue,
	type PkgHealthIssueKind,
	type PkgHealthRemoveAllResult,
	type PkgHealthRemoveResult,
	type PkgKernelStatus,
} from '@/lib/tauri-cmd';
import { agentUnavailableText } from '@/lib/agent-unavailable';
import { handToChi } from '@/shell/companion/companion-store';
import { ngwaSnapshotQueryKey } from '@/lib/ngwa/use-ngwa-snapshot';
import {
	ENGINE_IDS,
	NgwaConfirmDialog,
	buildRows,
	engineMark,
	errText,
	installedEngines,
	type ConfirmRequest,
} from './ngwa-scopes-surface';
import './ngwa.css';

export type HealthSection = 'violations' | 'sidecars' | 'cron' | 'data' | 'trust' | 'engines';

export interface NgwaHealthSurfaceProps {
	items: NgwaItem[];
	snapshot: Pick<NgwaSnapshot, 'as_of_ms' | 'sources'> | null;
	isLoading?: boolean;
	error?: Error | null;
	unreadableSources?: Array<{ source: string; error: string | null }>;
	section?: HealthSection;
	onOpenBackup: () => void;
	onOpenStore: () => void;
	/** Is this pkg id in the registry? Gates "Reinstall from registry". */
	canReinstall?: (pkgId: string) => boolean;
	/** D-02 "Reinstall from registry": open the pkg's Store sheet, where the
	 *  shared registry install path runs behind the consent step. */
	onReinstall?: (pkgId: string) => void;
	/** Injectable clock so "Audited N min ago" is testable. */
	now?: () => number;
}

export const HEALTH_KEYS = {
	violations: ['pkg', 'violations-audit'] as const,
	installs: ['pkg', 'health'] as const,
	kernel: ['pkg-kernel-status'] as const,
	agentOps: ['agent-ops', 'jobs'] as const,
	orphans: ['data', 'health'] as const,
	backups: ['backups', 'list'] as const,
	probe: (id: string) => ['ngwa', 'engine-probe', id] as const,
};

const SNAPSHOT_WAIT = 'Reading the snapshot… the first rescan can take 1–2 minutes.';

/** The agent id `detectAgent` understands for each engine column. */
const PROBE_ID: Record<EngineId, string> = { claude: 'claude-code', codex: 'codex', gemini: 'gemini' };

// ─── helpers ────────────────────────────────────────────────────────────────

export function fmtTime(ms: number | null | undefined): string {
	if (ms === null || ms === undefined || !Number.isFinite(ms)) return '—';
	return new Date(ms).toISOString().slice(0, 19).replace('T', ' ');
}

export function fmtBytes(n: number | null): string {
	if (n === null) return 'absent';
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
	return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function violationScopeLabel(kind: string): string {
	switch (kind) {
		case 'shell.execute':
			return 'spawn';
		case 'http':
			return 'host.fetch';
		case 'invoke':
			return 'host.invoke';
		default:
			return kind;
	}
}

export function issueLabel(kind: PkgHealthIssueKind): string {
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

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

export type Notice = { tone: 'ok' | 'err'; text: string };

/** The confirm for one install-health row's Remove. A pkgs-dir entry that
 *  failed to load has no record — the pkg is its folder — so Remove retires
 *  that folder to a recoverable backup (the uninstall path); every other kind
 *  is a DB-record purge. */
export function removeRequest(r: PkgHealthIssue, reinstallable: boolean): ConfirmRequest {
	if (r.issue.kind === 'pkgs_dir_unloadable') {
		return {
			title: `Remove ${r.id}`,
			confirmLabel: 'Remove',
			cancelLabel: 'Keep it',
			body: (
				<>
					<p>
						<code>{r.id}</code> failed to load, so it has no install record. Removing it moves its folder{' '}
						<code>{r.install_path}</code> to a <code>.uninstalled-…</code> backup in the pkgs folder, the
						same as an uninstall.
					</p>
					<p data-remove-recoverable>
						Nothing is deleted now: boot skips the backup and prunes it after 7 days, so it can be put
						back until then.
					</p>
					{reinstallable && (
						<p data-remove-reinstall-hint>To keep it, use Reinstall from registry instead.</p>
					)}
				</>
			),
			run: () => pkgHealthRemove(r.id),
		};
	}
	return {
		title: `Remove record ${r.id}`,
		confirmLabel: 'Remove',
		body: (
			<>
				<p>
					Deletes the {issueLabel(r.issue)} record <code>{r.id}</code>
					{r.install_path ? (
						<>
							{' '}
							(<code>{r.install_path}</code>)
						</>
					) : null}
					: its <code>pkg_installed</code> row and child <code>pkg_*</code> rows, or the orphan row.
				</p>
				<p>Files on disk are never touched. There is no undo.</p>
			</>
		),
		run: () => pkgHealthRemove(r.id),
	};
}

/** The result line for one row's Remove, from what the kernel actually did. */
export function summarizeRemove(id: string, r: PkgHealthRemoveResult | null | undefined): Notice {
	if (!r) return { tone: 'ok', text: `Removed ${id}` };
	const parts: string[] = [];
	if (r.removed_rows > 0) parts.push(`deleted ${plural(r.removed_rows, 'record row')}`);
	for (const f of r.retired) {
		parts.push(
			f.backup
				? `moved its folder to ${f.backup}`
				: 'set its manifest aside; the folder moves to a backup at next start'
		);
	}
	return { tone: 'ok', text: `Removed ${id}: ${parts.join(' · ') || 'nothing matched'}` };
}

/** The result line for Remove all: exactly what was removed, and what is still
 *  listed after the kernel's rescan and why. "ok" only when nothing is left. */
export function summarizeRemoveAll(r: PkgHealthRemoveAllResult): Notice {
	const done: string[] = [];
	if (r.removed_records > 0) done.push(plural(r.removed_records, 'record'));
	if (r.retired_folders.length > 0) done.push(`retired ${plural(r.retired_folders.length, 'folder')}`);
	if (r.removed_orphans > 0) done.push(plural(r.removed_orphans, 'orphan row'));
	const head = done.length ? `Removed ${done.join(' · ')}` : '0 removed';
	if (r.rescan_error) {
		return { tone: 'err', text: `${head} — the rescan failed, so what is left is unknown: ${r.rescan_error}` };
	}
	const failedById = new Map(r.failed.map((f) => [f.id, f.error]));
	const leftIds = Array.from(new Set(r.remaining.map((i) => i.id)));
	const why = leftIds.map((id) => {
		const err = failedById.get(id);
		if (err) return `${id}: ${err}`;
		const kinds = r.remaining.filter((i) => i.id === id).map((i) => i.issue.kind);
		return kinds.includes('pkgs_dir_unloadable') || kinds.includes('register_failed')
			? `${id} needs Reinstall or Remove`
			: `${id} is still listed`;
	});
	// A failure whose id is no longer listed is still named.
	for (const f of r.failed) if (!leftIds.includes(f.id)) why.push(`${f.id}: ${f.error}`);
	if (why.length === 0) return { tone: 'ok', text: head };
	return {
		tone: 'err',
		text: `${head} — ${leftIds.length ? `${plural(leftIds.length, 'issue')} left: ` : ''}${why.join('; ')}`,
	};
}

interface RegistryRead<T> {
	entries: T[] | null;
	error: string | null;
}

function readRegistry<T>(status: PkgKernelStatus | undefined, name: string): RegistryRead<T> {
	if (!status) return { entries: null, error: null };
	const reg = status.registries?.[name] as Record<string, unknown> | undefined;
	if (!reg || typeof reg !== 'object') {
		return { entries: null, error: `${name} registry missing from kernel status` };
	}
	if (typeof reg.error === 'string') return { entries: null, error: reg.error };
	if (!Array.isArray(reg.entries)) {
		return { entries: null, error: `${name} snapshot has no entries array` };
	}
	return { entries: reg.entries as T[], error: null };
}

interface SidecarEntry {
	pkg_id: string;
	state: string;
	pid: number | null;
	uptime_s: number | null;
	restarts: number;
	last_crash_unix_ms: number | null;
	last_err: string | null;
}

interface PkgCronEntry {
	pkg_id: string;
	cron_id: string;
	expr: string;
	handler: string;
}

function Err({ children }: { children: ReactNode }) {
	return (
		<div className="hnote herr" role="alert">
			<AlertTriangle className="inline h-3.5 w-3.5" /> {children}
		</div>
	);
}

// ─── Surface ────────────────────────────────────────────────────────────────

export function NgwaHealthSurface({
	items,
	snapshot,
	isLoading = false,
	error = null,
	unreadableSources = [],
	section,
	onOpenBackup,
	onOpenStore,
	canReinstall,
	onReinstall,
	now = Date.now,
}: NgwaHealthSurfaceProps) {
	const qc = useQueryClient();
	const [confirm, setConfirm] = useState<ConfirmRequest | null>(null);
	const [notice, setNotice] = useState<{ tone: 'ok' | 'err'; text: string } | null>(null);
	const [showUnsigned, setShowUnsigned] = useState(false);
	const [scanned, setScanned] = useState(false);
	const [logFor, setLogFor] = useState<string | null>(null);
	const [runMsg, setRunMsg] = useState<Record<string, { tone: 'ok' | 'err'; text: string }>>({});
	const [restartMsg, setRestartMsg] = useState<Record<string, { tone: 'ok' | 'err'; text: string }>>(
		{}
	);

	// ── queries (each independent of the snapshot) ──
	const violationsQ = useQuery({
		queryKey: HEALTH_KEYS.violations,
		queryFn: () => pkgPermissionViolationsList(undefined, 200),
		retry: false,
	});
	const installsQ = useQuery({ queryKey: HEALTH_KEYS.installs, queryFn: pkgHealthScan, retry: false });
	const kernelQ = useQuery({ queryKey: HEALTH_KEYS.kernel, queryFn: pkgKernelStatus, retry: false });
	const agentOpsQ = useQuery({
		queryKey: HEALTH_KEYS.agentOps,
		queryFn: async () => {
			const r = (await agentOpsListJobs()) as AgentOpsListJobsResult;
			if (!r || typeof r !== 'object') throw new Error('agent-ops returned no result');
			if (!r.ok) throw new Error(`${r.code}: ${r.error}`);
			return r;
		},
		retry: false,
	});
	const orphansQ = useQuery({
		queryKey: HEALTH_KEYS.orphans,
		queryFn: dataHealthScan,
		enabled: scanned,
		retry: false,
	});
	const backupsQ = useQuery({ queryKey: HEALTH_KEYS.backups, queryFn: backupList, retry: false });
	const probes = useQueries({
		queries: ENGINE_IDS.map((e) => ({
			queryKey: HEALTH_KEYS.probe(PROBE_ID[e]),
			queryFn: () => detectAgent(PROBE_ID[e]),
			staleTime: 5 * 60_000,
			retry: false,
		})),
	});

	const dbSize = useMutation<DbFileSizes, Error>({ mutationFn: () => dataHealthDbSize() });
	const tail = useMutation<AgentOpsTailRunResult, Error, string>({
		mutationFn: (jobId) => agentOpsTailRun(jobId, 0),
	});

	// ── derived ──
	const sidecars = readRegistry<SidecarEntry>(kernelQ.data, 'sidecar_supervisor');
	const pkgCron = readRegistry<PkgCronEntry>(kernelQ.data, 'cron');
	const violations = violationsQ.data ?? [];
	const installs: PkgHealthIssue[] = installsQ.data ?? [];
	const snapshotReady = !isLoading && !error && snapshot !== null;
	const unsigned = useMemo(
		() => items.filter((it) => it.trust.signed === false && it.trust.state !== 'needs_approval'),
		[items]
	);
	const trustDown = unreadableSources.filter((s) =>
		['trust', 'kernel', 'oba', 'engine_config'].includes(s.source)
	);
	const engines = useMemo(() => installedEngines(items), [items]);
	const rows = useMemo(() => buildRows(items), [items]);

	// ── ?section= → scroll to and focus that panel ──
	const panelRefs = useRef<Partial<Record<HealthSection, HTMLElement | null>>>({});
	useEffect(() => {
		if (!section) return;
		const el = panelRefs.current[section];
		if (!el) return;
		el.scrollIntoView?.({ block: 'start' });
		el.focus({ preventScroll: true });
	}, [section]);
	const panelProps = (s: HealthSection) => ({
		ref: (el: HTMLElement | null) => {
			panelRefs.current[s] = el;
		},
		tabIndex: -1,
		'data-panel': s,
		'aria-current': section === s ? ('true' as const) : undefined,
	});

	const lastResult = useRef<unknown>(null);
	function done(
		label: string,
		invalidate: readonly (readonly unknown[])[],
		summarize?: (result: unknown) => Notice
	) {
		return (result: { ok: true } | { ok: false; error: string } | null) => {
			setConfirm(null);
			if (!result) return;
			for (const k of invalidate) void qc.invalidateQueries({ queryKey: k });
			setNotice(
				!result.ok
					? { tone: 'err', text: `${label} failed: ${result.error}` }
					: summarize
						? summarize(lastResult.current)
						: { tone: 'ok', text: `${label}: done` }
			);
		};
	}
	const [onConfirmClose, setOnConfirmClose] = useState<
		((r: { ok: true } | { ok: false; error: string } | null) => void) | null
	>(null);
	function ask(
		req: ConfirmRequest,
		label: string,
		invalidate: readonly (readonly unknown[])[],
		summarize?: (result: unknown) => Notice
	) {
		lastResult.current = null;
		setConfirm({
			...req,
			run: async () => {
				lastResult.current = await req.run();
			},
		});
		setOnConfirmClose(() => done(label, invalidate, summarize));
	}

	function askRemove(r: PkgHealthIssue) {
		const reinstallable = isUnregisteredPkgIssue(r.issue) && !!onReinstall && !!canReinstall?.(r.id);
		ask(
			removeRequest(r, reinstallable),
			`Remove ${r.id}`,
			[HEALTH_KEYS.installs, ngwaSnapshotQueryKey],
			(res) => summarizeRemove(r.id, res as PkgHealthRemoveResult | null)
		);
	}

	function askRemoveAll() {
		const folders = installs.filter((r) => r.issue.kind === 'pkgs_dir_unloadable');
		const reinstallable = folders.filter((r) => onReinstall && canReinstall?.(r.id));
		ask(
			{
				title: 'Remove all listed issues',
				confirmLabel: 'Remove all',
				body: (
					<>
						<p data-removeall-rescan>
							The kernel <b>rescans when you confirm</b> and removes whatever is broken or orphaned{' '}
							<b>at that moment</b>: each broken <code>pkg_installed</code> row with its child{' '}
							<code>pkg_*</code> rows, each orphan row, and each pkgs-folder entry that failed to load.
							That set can differ from the list below if anything changed since this screen last scanned.
						</p>
						<p>
							Last scan found {installs.length}: {installs.map((r) => r.id).join(', ')}
						</p>
						{folders.length > 0 && (
							<p data-removeall-folders>
								{folders.length === 1 ? 'The folder' : `The ${folders.length} folders`} (
								{folders.map((r) => r.id).join(', ')}) {folders.length === 1 ? 'is' : 'are'} not deleted:
								each moves to a <code>.uninstalled-…</code> backup in the pkgs folder, which boot skips
								and prunes after 7 days.
								{reinstallable.length > 0 && (
									<> To keep {reinstallable.map((r) => r.id).join(', ')}, cancel and use Reinstall from registry.</>
								)}
							</p>
						)}
						<p>Deleted records have no undo. You get a line saying exactly what was removed and what is left.</p>
					</>
				),
				run: () => pkgHealthRemoveAll(),
			},
			'Remove all',
			[HEALTH_KEYS.installs, ngwaSnapshotQueryKey],
			(res) => {
				const report = res as PkgHealthRemoveAllResult | null;
				if (!report) return { tone: 'err', text: 'Remove all returned no report' };
				// Show the kernel's own post-removal rescan at once; the
				// invalidation above then refetches it.
				if (!report.rescan_error) qc.setQueryData(HEALTH_KEYS.installs, report.remaining);
				return summarizeRemoveAll(report);
			}
		);
	}

	// ── auditline ──
	const readSources: string[] = [];
	if (violationsQ.isSuccess) readSources.push('permission audit');
	if (installsQ.isSuccess) readSources.push('install records');
	if (kernelQ.isSuccess && sidecars.entries) readSources.push('sidecar supervisor');
	if (kernelQ.isSuccess && pkgCron.entries) readSources.push('pkg cron registry');
	if (agentOpsQ.isSuccess) readSources.push('agent-ops');
	if (dbSize.isSuccess) readSources.push('SQLite file sizes');
	if (orphansQ.isSuccess) readSources.push('orphan scan');
	if (backupsQ.isSuccess) readSources.push('backups folder');
	const probed = ENGINE_IDS.filter((_, i) => probes[i]?.isSuccess);
	if (probed.length) readSources.push(`engine probes (${probed.join(', ')})`);
	const snapshotSourcesOk = snapshot
		? (Object.entries(snapshot.sources) as Array<[string, { ok: boolean }]>)
				.filter(([, h]) => h.ok)
				.map(([k]) => k)
		: [];
	const failedSources: string[] = [];
	if (snapshot) {
		for (const [k, h] of Object.entries(snapshot.sources) as Array<[string, { ok: boolean }]>) {
			if (!h.ok) failedSources.push(k);
		}
	}
	if (violationsQ.isError) failedSources.push('permission audit');
	if (installsQ.isError) failedSources.push('install records');
	if (kernelQ.isError || (kernelQ.isSuccess && (sidecars.error || pkgCron.error))) {
		failedSources.push('kernel registries');
	}
	if (agentOpsQ.isError) failedSources.push('agent-ops');
	if (dbSize.isError) failedSources.push('SQLite file sizes');
	if (orphansQ.isError) failedSources.push('orphan scan');
	if (backupsQ.isError) failedSources.push('backups folder');
	ENGINE_IDS.forEach((e, i) => {
		if (probes[i]?.isError) failedSources.push(`engine probe (${e})`);
	});
	const auditOk = !error && snapshot !== null && failedSources.length === 0;
	const ageMin = snapshot ? Math.max(0, Math.floor((now() - snapshot.as_of_ms) / 60_000)) : null;

	const violationCount = violationsQ.isSuccess && installsQ.isSuccess ? violations.length + installs.length : null;
	const agentJobs: AgentOpsRawJob[] = agentOpsQ.data?.jobs ?? [];
	const cronCount =
		agentOpsQ.isSuccess && pkgCron.entries ? agentJobs.length + pkgCron.entries.length : null;
	const newestBackup = (backupsQ.data ?? [])
		.slice()
		.sort((a, b) => (b.created_at || '').localeCompare(a.created_at || ''))[0];

	return (
		<div className="view-ngwa flex-1 min-h-0 flex flex-col">
			{notice && (
				<div className={`mstatus ${notice.tone}`} role="status" data-hnotice>
					{notice.text}
				</div>
			)}
			<div className="hscroll sc" data-hscroll>
			<div className="hgrid" data-hgrid>
				{/* ── 1. Violations ── */}
				<section className="panel" {...panelProps('violations')} aria-label="Violations">
					<h3>
						<AlertTriangle className="h-3.5 w-3.5" />
						<span>Violations</span>
						{violationCount !== null && (
							<span className={`n ${violationCount > 0 ? 'bad' : 'ok'}`} data-violn>
								{violationCount}
							</span>
						)}
					</h3>

					{/* install integrity: broken / unregistered pkgs first — they are what blocks a view */}
					{installsQ.isLoading ? (
						<div className="hnote">Scanning installs…</div>
					) : installsQ.error ? (
						<Err>Health scan failed: {errText(installsQ.error)}</Err>
					) : installs.length === 0 ? (
						<div className="hnote" data-empty="installs">
							All install records healthy.
						</div>
					) : (
						<div className="hlist" data-list="installs">
							{installs.map((r) => {
								const reinstall =
									isUnregisteredPkgIssue(r.issue) && onReinstall && canReinstall?.(r.id);
								return (
									<div key={`${r.id}:${r.issue.kind}`} className="hrow" data-install={r.id}>
										<AlertTriangle className="hico h-4 w-4 flex-none" />
										<div className="txt">
											<span className="t1">
												{r.id}{' '}
												<span
													className={`tag ${r.issue.kind === 'orphan_row' ? '' : 'bad'}`}
													data-issue={r.issue.kind}
												>
													{issueLabel(r.issue)}
												</span>
												{!r.enabled && r.issue.kind !== 'pkgs_dir_unloadable' && (
													<span className="tag">disabled</span>
												)}
											</span>
											<span className="t2">
												{r.detail}
												{r.install_path ? (
													<>
														{' '}
														· <code>{r.install_path}</code>
													</>
												) : null}
											</span>
										</div>
										<div className="acts">
											{reinstall && (
												<button
													type="button"
													className="chip on"
													data-reinstall={r.id}
													title="Fetch it again from the signed registry. The Store sheet asks for consent first."
													onClick={() => onReinstall(r.id)}
												>
													Reinstall from registry
												</button>
											)}
											<button
												type="button"
												className="chip danger"
												data-remove={r.id}
												title={
													r.issue.kind === 'pkgs_dir_unloadable'
														? 'Move its folder to a recoverable backup'
														: 'Delete this record'
												}
												onClick={() => askRemove(r)}
											>
												Remove…
											</button>
											<button
												type="button"
												className="chip"
												data-chi={r.id}
												onClick={() =>
													handToChi(
														`Look at the Ikenga pkg ${r.id} (${issueLabel(r.issue)})${
															r.install_path ? ` at ${r.install_path}` : ''
														}: ${r.detail}`
													)
												}
											>
												Hand to Chi
											</button>
										</div>
									</div>
								);
							})}
						</div>
					)}

					{/* permission violations */}
					{violationsQ.isLoading ? (
						<div className="hnote">Loading violations…</div>
					) : violationsQ.error ? (
						<Err>Failed to load violations: {errText(violationsQ.error)}</Err>
					) : violations.length === 0 ? (
						<div className="hnote" data-empty="violations">
							No permission violations recorded.
						</div>
					) : (
						<div className="hlist" data-list="violations">
							{violations.map((v) => {
								const n = violations.filter((x) => x.pkg_id === v.pkg_id).length;
								return (
									<div key={v.id} className="hrow" data-violation={v.id}>
										<AlertTriangle className="hico h-4 w-4 flex-none" />
										<div className="txt">
											<span className="t1">
												{v.pkg_id} attempted <code>{v.attempted}</code>{' '}
												<span className="tag">{violationScopeLabel(v.scope_kind)}</span>
											</span>
											<span className="t2">
												declared <code>{v.declared || '—'}</code> · {fmtTime(v.occurred_at)}
											</span>
										</div>
										<div className="acts">
											<button
												type="button"
												className="chip"
												data-clear={v.pkg_id}
												title={`Clear the ${plural(n, 'audit row')} for ${v.pkg_id}`}
												onClick={() =>
													ask(
														{
															title: `Clear violations for ${v.pkg_id}`,
															confirmLabel: 'Clear',
															body: (
																<>
																	<p>
																		Deletes the {plural(n, 'audit row')} for <code>{v.pkg_id}</code> from{' '}
																		<code>pkg_permission_violations</code>.
																	</p>
																	<p>Audit-only: trust state and grants are unchanged. There is no undo.</p>
																</>
															),
															run: () => pkgPermissionViolationsClear(v.pkg_id),
														},
														`Clear violations for ${v.pkg_id}`,
														[HEALTH_KEYS.violations]
													)
												}
											>
												Clear
											</button>
											<button
												type="button"
												className="chip"
												data-chi={`violation:${v.id}`}
												onClick={() =>
													handToChi(
														`Look at why ${v.pkg_id} attempted ${violationScopeLabel(v.scope_kind)} ${v.attempted} when it declared ${v.declared || 'nothing'}`
													)
												}
											>
												Hand to Chi
											</button>
										</div>
									</div>
								);
							})}
						</div>
					)}

					<div className="panelfoot" data-violations-foot>
						<span className="grow">
							{violationsQ.isSuccess && violations.length > 0
								? `Showing the ${violations.length} most recent permission violations (cap 200). Local audit data only.`
								: 'Install scan and local permission audit.'}
						</span>
						<button
							type="button"
							className="chip"
							aria-label="Refresh violations"
							title="Refresh violations"
							disabled={violationsQ.isFetching}
							onClick={() => void violationsQ.refetch()}
						>
							<RefreshCw className="h-3 w-3" />
						</button>
						<button
							type="button"
							className="chip"
							aria-label="Rescan install records"
							title="Rescan install records"
							disabled={installsQ.isFetching}
							onClick={() => void installsQ.refetch()}
						>
							Rescan
						</button>
						{installs.length > 0 && (
							<button type="button" className="chip danger" data-act="remove-all" onClick={askRemoveAll}>
								Remove all…
							</button>
						)}
					</div>
				</section>

				{/* ── 2. Sidecars ── */}
				<section className="panel" {...panelProps('sidecars')} aria-label="Sidecars">
					<h3>
						<Play className="h-3.5 w-3.5" />
						<span>Sidecars</span>
						<span className="n" data-sidecarn>
							{sidecars.entries ? sidecars.entries.length : '—'}
						</span>
					</h3>
					{kernelQ.isLoading ? (
						<div className="hnote">Reading the supervisor…</div>
					) : kernelQ.error ? (
						<Err>Kernel status failed: {errText(kernelQ.error)}</Err>
					) : sidecars.error ? (
						<Err>{sidecars.error}</Err>
					) : sidecars.entries && sidecars.entries.length === 0 ? (
						<div className="hnote">No supervised sidecars.</div>
					) : (
						sidecars.entries?.map((e) => {
							const canRestart = e.state !== 'running' && e.state !== 'spawning';
							const msg = restartMsg[e.pkg_id];
							return (
								<div key={e.pkg_id} className="hrow" data-sidecar={e.pkg_id}>
									<div className="txt">
										<span className="t1">{e.pkg_id}</span>
										<span className="t2">
											{e.restarts} restart{e.restarts === 1 ? '' : 's'} · pid {e.pid ?? '—'} · up{' '}
											{e.uptime_s === null ? '—' : `${e.uptime_s}s`} · last crash{' '}
											{fmtTime(e.last_crash_unix_ms)}
											{e.last_err ? ` · ${e.last_err}` : ''}
										</span>
										{msg && <span className={`t2 ${msg.tone === 'err' ? 'herr' : ''}`}>{msg.text}</span>}
									</div>
									<div className="acts">
										<span className={`state s-${e.state}`}>{e.state}</span>
										<button
											type="button"
											className="chip"
											disabled
											title="No sidecar log source is exposed to the shell yet"
										>
											Logs
										</button>
										{canRestart && (
											<button
												type="button"
												className="chip"
												data-restart={e.pkg_id}
												onClick={async () => {
													try {
														const supervised = await pkgSupervisorRestart(e.pkg_id);
														setRestartMsg((m) => ({
															...m,
															[e.pkg_id]: supervised
																? { tone: 'ok', text: 'Restart requested' }
																: { tone: 'err', text: 'Not supervised here' },
														}));
													} catch (err) {
														setRestartMsg((m) => ({
															...m,
															[e.pkg_id]: { tone: 'err', text: `Restart failed: ${errText(err)}` },
														}));
													}
													void qc.invalidateQueries({ queryKey: HEALTH_KEYS.kernel });
													void qc.invalidateQueries({ queryKey: ngwaSnapshotQueryKey });
												}}
											>
												{e.state === 'parked' ? 'Start' : 'Restart'}
											</button>
										)}
									</div>
								</div>
							);
						})
					)}
				</section>

				{/* ── 3. Cron (DEC-33) ── */}
				<section className="panel" {...panelProps('cron')} aria-label="Cron">
					<h3>
						<Clock className="h-3.5 w-3.5" />
						<span>Cron</span>
						<span className="n" data-cronn>
							{cronCount ?? '—'}
						</span>
					</h3>
					<div className="hsub">
						<span>agent-ops jobs</span>
						<span className="rt" data-daemon>
							{agentOpsQ.data
								? agentOpsQ.data.daemon_up
									? `daemon running (pid ${agentOpsQ.data.daemon_pid ?? '—'})`
									: 'daemon not running'
								: '—'}
						</span>
					</div>
					{agentOpsQ.isLoading ? (
						<div className="hnote">Reading agent-ops…</div>
					) : agentOpsQ.error ? (
						<Err>agent-ops unreadable: {errText(agentOpsQ.error)}</Err>
					) : agentJobs.length === 0 ? (
						<div className="hnote">No agent-ops jobs configured.</div>
					) : (
						agentJobs.map((j) => {
							const msg = runMsg[j.id];
							const daemonUp = agentOpsQ.data?.daemon_up === true;
							return (
								<div key={j.id} data-job={j.id}>
									<div className="hrow">
										<div className="txt">
											<span className="t1">
												{j.label || j.id} <span className="tag info">agent-ops</span>
												{!j.enabled && <span className="tag">disabled</span>}
											</span>
											<span className="t2">
												{j.schedule} ({j.timezone}) · last run {fmtTime(j.state?.lastRunAtMs)}
												{j.state?.lastStatus ? ` ${j.state.lastStatus}` : ''} · next{' '}
												{fmtTime(j.state?.nextRunAtMs)}
											</span>
											{msg && <span className={`t2 ${msg.tone === 'err' ? 'herr' : ''}`}>{msg.text}</span>}
										</div>
										<div className="acts">
											<button
												type="button"
												className="chip"
												data-runnow={j.id}
												disabled={!daemonUp}
												title={daemonUp ? undefined : 'The agent-ops daemon is not running'}
												onClick={async () => {
													try {
														const r = (await agentOpsRunNow(j.id)) as AgentOpsRunNowResult;
														setRunMsg((m) => ({
															...m,
															[j.id]: r.ok
																? { tone: 'ok', text: `Triggered: ${r.message}` }
																: { tone: 'err', text: `Run failed: ${r.code}: ${r.error}` },
														}));
													} catch (err) {
														setRunMsg((m) => ({
															...m,
															[j.id]: { tone: 'err', text: `Run failed: ${errText(err)}` },
														}));
													}
													void qc.invalidateQueries({ queryKey: HEALTH_KEYS.agentOps });
												}}
											>
												Run now
											</button>
											<button
												type="button"
												className="chip"
												data-logs={j.id}
												aria-expanded={logFor === j.id}
												onClick={() => {
													if (logFor === j.id) {
														setLogFor(null);
														return;
													}
													setLogFor(j.id);
													tail.mutate(j.id);
												}}
											>
												Logs
											</button>
										</div>
									</div>
									{logFor === j.id && (
										<pre className="hlog" data-log={j.id}>
											{tail.isPending
												? 'Reading…'
												: tail.error
													? `Log read failed: ${errText(tail.error)}`
													: tail.data && !tail.data.ok
														? `Log read failed: ${tail.data.code}: ${tail.data.error}`
														: tail.data && tail.data.ok
															? tail.data.mode === 'agent'
																? 'Agent-mode jobs keep no tail output.'
																: tail.data.chunk || 'No output recorded for the last run.'
															: '—'}
										</pre>
									)}
								</div>
							);
						})
					)}
					<div className="hsub">
						<span>pkg manifest cron</span>
						<span className="rt">no run history</span>
					</div>
					{kernelQ.isLoading ? (
						<div className="hnote">Reading the cron registry…</div>
					) : kernelQ.error ? (
						<Err>Kernel status failed: {errText(kernelQ.error)}</Err>
					) : pkgCron.error ? (
						<Err>{pkgCron.error}</Err>
					) : pkgCron.entries && pkgCron.entries.length === 0 ? (
						<div className="hnote">No pkg declares a cron entry.</div>
					) : (
						pkgCron.entries?.map((c) => (
							<div key={`${c.pkg_id}::${c.cron_id}`} className="hrow" data-pkgcron={`${c.pkg_id}::${c.cron_id}`}>
								<div className="txt">
									<span className="t1">
										{c.pkg_id} · {c.cron_id} <span className="tag">pkg cron</span>
									</span>
									<span className="t2">
										<code>{c.expr}</code> → <code>{c.handler}</code> · last run —
									</span>
								</div>
							</div>
						))
					)}
				</section>

				{/* ── 4. Data ── */}
				<section className="panel" {...panelProps('data')} aria-label="Data">
					<h3>
						<Database className="h-3.5 w-3.5" />
						<span>Data</span>
						<span className="n" data-datan>
							{orphansQ.isSuccess
								? `${orphansQ.data.reduce((n, r) => n + r.orphan_count, 0)} orphans`
								: 'on demand'}
						</span>
					</h3>

					<div className="hrow" data-row="dbsize">
						<div className="txt">
							<span className="t1">Database size</span>
							<span className="t2">
								{dbSize.isSuccess ? (
									<>
										Measured just now from <code>{dbSize.data.db_path}</code> · wal{' '}
										{fmtBytes(dbSize.data.wal_bytes)} · shm {fmtBytes(dbSize.data.shm_bytes)}
									</>
								) : dbSize.error ? (
									<span className="herr">Measure failed: {errText(dbSize.error)}</span>
								) : (
									'Measured on demand; the snapshot does not carry it.'
								)}
							</span>
						</div>
						<div className="acts">
							<span className="meta mono" data-slot="dbsize">
								{dbSize.isSuccess ? fmtBytes(dbSize.data.db_bytes) : '—'}
							</span>
							<button
								type="button"
								className="chip"
								data-act="measure"
								disabled={dbSize.isPending}
								aria-busy={dbSize.isPending || undefined}
								onClick={() => dbSize.mutate()}
							>
								{dbSize.isPending ? 'Measuring…' : 'Measure'}
							</button>
						</div>
					</div>

					<div className="hrow" data-row="orphans">
						<div className="txt">
							<span className="t1">Orphan rows</span>
							<span className="t2">
								Soft links whose parent row is gone. The scan only reads; repair records in their owning
								app.
							</span>
						</div>
						<div className="acts">
							<span className="meta mono" data-slot="orphans">
								{orphansQ.isSuccess ? orphansQ.data.reduce((n, r) => n + r.orphan_count, 0) : '—'}
							</span>
							<button
								type="button"
								className="chip"
								data-act="scan"
								disabled={orphansQ.isFetching}
								aria-busy={orphansQ.isFetching || undefined}
								onClick={() => {
									if (!scanned) setScanned(true);
									else void orphansQ.refetch();
								}}
							>
								{orphansQ.isFetching ? 'Scanning…' : scanned ? 'Rescan' : 'Scan'}
							</button>
						</div>
					</div>
					{orphansQ.error ? (
						<Err>Data-health scan failed: {errText(orphansQ.error)}</Err>
					) : orphansQ.isSuccess && orphansQ.data.length === 0 ? (
						<div className="hnote" data-empty="orphans">
							No orphaned references: every audited soft link resolves.
						</div>
					) : orphansQ.isSuccess ? (
						<div className="hlist" data-list="orphans">
							{orphansQ.data.map((r) => (
								<div key={`${r.table}.${r.column}`} className="hrow" data-orphan={`${r.table}.${r.column}`}>
									<div className="txt">
										<span className="t1">
											<code>
												{r.table}.{r.column}
											</code>{' '}
											→ <code>{r.parent_table}.id</code> · {r.orphan_count} dangling
										</span>
										<span className="t2">
											{r.sample_ids.map((id) => (
												<code key={id} className="mr-1">
													{id}
												</code>
											))}
											{r.orphan_count > r.sample_ids.length && ` +${r.orphan_count - r.sample_ids.length} more`}
										</span>
									</div>
								</div>
							))}
						</div>
					) : null}

					<div className="hrow" data-row="backup">
						<div className="txt">
							<span className="t1">Last backup</span>
							<span className="t2">
								{backupsQ.isLoading ? (
									'Reading the backups folder…'
								) : backupsQ.error ? (
									<span className="herr">Backup list failed: {errText(backupsQ.error)}</span>
								) : newestBackup ? (
									<>
										Newest in the local backups folder: <code>{newestBackup.path}</code>
									</>
								) : (
									'No backup in the local backups folder.'
								)}
							</span>
						</div>
						<div className="acts">
							<span className="meta mono" data-slot="backup">
								{newestBackup?.created_at ? newestBackup.created_at : '—'}
							</span>
							<button type="button" className="chip" data-act="backup" onClick={onOpenBackup}>
								Back up now
							</button>
						</div>
					</div>
				</section>

				{/* ── 5. Trust ── */}
				<section className="panel" {...panelProps('trust')} aria-label="Trust">
					<h3>
						<Shield className="h-3.5 w-3.5" />
						<span>Trust</span>
						<span className="n warn" data-unsignedn>
							{snapshotReady && trustDown.length === 0 ? unsigned.length : '—'} unsigned
						</span>
					</h3>
					<div className="hrow" data-row="unsigned">
						<Shield className="hico h-4 w-4 flex-none" />
						<div className="txt">
							{isLoading ? (
								<span className="t1" data-snapshot-loading>
									{SNAPSHOT_WAIT}
								</span>
							) : error ? (
								<span className="t1 herr">Snapshot failed: {error.message}</span>
							) : trustDown.length > 0 ? (
								<span className="t1 herr" data-unsigned-unknown>
									Unsigned count unknown: {trustDown.map((s) => s.source).join(', ')} unreadable.
								</span>
							) : (
								<>
									<span className="t1" data-unsigned>
										{unsigned.length} of {items.length} installed items are unsigned
									</span>
									<span className="t2">
										Unsigned means no signature and no approval pending. Skills, agents, commands and hooks
										carry no manifest, so they can never be signed; pkgs are unsigned when their manifest
										has no signature.
									</span>
								</>
							)}
						</div>
						<div className="acts">
							<button
								type="button"
								className="chip"
								data-act="review-unsigned"
								aria-expanded={showUnsigned}
								disabled={!snapshotReady || trustDown.length > 0 || unsigned.length === 0}
								title={
									!snapshotReady
										? 'Waiting for the snapshot'
										: trustDown.length > 0
											? 'Trust data is unreadable'
											: unsigned.length === 0
												? 'Nothing unsigned'
												: undefined
								}
								onClick={() => setShowUnsigned((v) => !v)}
							>
								Review the {snapshotReady && trustDown.length === 0 ? unsigned.length : '—'}
							</button>
						</div>
					</div>
					{showUnsigned && (
						<div className="hlist" data-list="unsigned">
							{unsigned.map((it) => (
								<div key={it.id} className="hrow">
									<div className="txt">
										<span className="t1">
											{it.display_name || it.name} <span className="tag">{it.kind}</span>
										</span>
										<span className="t2">
											source {it.origin.source} · trust {it.trust.state}
										</span>
									</div>
								</div>
							))}
						</div>
					)}
				</section>

				{/* ── 6. Engines ── */}
				<section className="panel" {...panelProps('engines')} aria-label="Engines">
					<h3>
						<Bot className="h-3.5 w-3.5" />
						<span>Engines</span>
						<span className="n" data-enginen>
							{snapshotReady ? `${engines.size} of ${ENGINE_IDS.length}` : '—'}
						</span>
					</h3>
					{ENGINE_IDS.map((eng, i) => {
						const probe = probes[i];
						const pkgItem = engines.get(eng);
						const placed = rows.filter((r) => engineMark(r, eng) !== 'none').length;
						const agent = probe?.data;
						return (
							<div key={eng} className="hrow" data-engine={eng}>
								<div className="txt">
									<span className="t1">
										{eng} <code>{agent?.version ?? '—'}</code>
									</span>
									<span className="t2" data-engine-pkg>
										{!snapshotReady
											? isLoading
												? SNAPSHOT_WAIT
												: 'Engine extensions unknown: the snapshot is unavailable.'
											: pkgItem
												? `engine extension ${pkgItem.name} ${pkgItem.version ?? '—'} · ${placed} items placed`
												: `No engine extension installed, so Scopes shows no ${eng} column · ${placed} items placed`}
									</span>
									<span className="t2" data-engine-probe>
										{probe?.isLoading
											? 'Probing the CLI…'
											: probe?.error
												? `CLI probe failed: ${errText(probe.error)}`
												: agentUnavailableText(agent)
													? `Couldn't check the CLI: ${agentUnavailableText(agent)}`
													: agent
													? `CLI at ${agent.executable_path} · auth ${
															agent.authed === null ? 'unknown' : agent.authed ? 'ok' : 'not signed in'
														}`
													: 'CLI not found by the probe'}
									</span>
								</div>
								<div className="acts">
									{snapshotReady && pkgItem ? (
										<span className={`state s-${pkgItem.state}`}>{pkgItem.state}</span>
									) : snapshotReady ? (
										<button
											type="button"
											className="chip"
											data-act="installengine"
											title="Opens the Store; engines are listed there"
											onClick={onOpenStore}
										>
											Open Store
										</button>
									) : null}
								</div>
							</div>
						);
					})}
				</section>

				{/* ── Auditline ── */}
				<div className="auditline" data-auditline data-audit-state={auditOk ? 'ok' : 'degraded'}>
					{auditOk ? (
						<CheckCircle className="h-3.5 w-3.5 ok" />
					) : (
						<AlertTriangle className="h-3.5 w-3.5 herr" />
					)}
					<span>
						{error ? (
							<span className="herr">Snapshot failed: {error.message}</span>
						) : snapshot && ageMin !== null ? (
							<>
								Audited {ageMin === 0 ? 'less than a minute' : `${ageMin} min`} ago from{' '}
								<span className="font-mono">ngwa_snapshot</span> ({snapshotSourcesOk.join(', ') || 'no source readable'})
							</>
						) : (
							<>Snapshot not read yet</>
						)}
						{failedSources.length > 0 && <> · could not read: {failedSources.join(', ')}</>}
						{readSources.length > 0 && <> · also read: {readSources.join(', ')}</>}. Anything not
						measured reads “—”.
					</span>
				</div>
				</div>
			</div>

			<NgwaConfirmDialog
				request={confirm}
				onClose={(r) => {
					if (onConfirmClose) onConfirmClose(r);
					else setConfirm(null);
				}}
			/>
		</div>
	);
}
