// Ngwa Health Surface (WP-16 / WP-16a / locked D-02 frame-workbench-v4.html).
//
// Five panels, each reading its own real source and rendering independently
// of the (slow) Ngwa snapshot:
//   1. Violations — `pkg_permission_violations` (full list, per-pkg Clear) and
//      the install-integrity scan (`pkgHealthScan`, Remove / Remove all), plus
//      the unsigned count from the snapshot (gate §5).
//   2. Sidecars  — `pkgKernelStatus().registries.sidecar_supervisor`, Restart.
//   3. Cron      — agent-ops jobs (`agentOpsListJobs`, Run now, Logs) and pkg
//      manifest `cron[]` (`registries.cron`, no run history) — DEC-33.
//   4. Data      — DB file sizes (DEC-32), soft-FK orphan scan, newest backup.
//   5. Engines   — the shared installed-engine signal + `detectAgent` probes.
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
	type PkgKernelStatus,
} from '@/lib/tauri-cmd';
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

export type HealthSection = 'violations' | 'sidecars' | 'cron' | 'data' | 'engines';

export interface NgwaHealthSurfaceProps {
	items: NgwaItem[];
	snapshot: Pick<NgwaSnapshot, 'as_of_ms' | 'sources'> | null;
	isLoading?: boolean;
	error?: Error | null;
	unreadableSources?: Array<{ source: string; error: string | null }>;
	section?: HealthSection;
	onOpenBackup: () => void;
	onOpenStore: () => void;
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
	}
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

	function done(label: string, invalidate: readonly (readonly unknown[])[]) {
		return (result: { ok: true } | { ok: false; error: string } | null) => {
			setConfirm(null);
			if (!result) return;
			for (const k of invalidate) void qc.invalidateQueries({ queryKey: k });
			setNotice(
				result.ok
					? { tone: 'ok', text: `${label}: done` }
					: { tone: 'err', text: `${label} failed: ${result.error}` }
			);
		};
	}
	const [onConfirmClose, setOnConfirmClose] = useState<
		((r: { ok: true } | { ok: false; error: string } | null) => void) | null
	>(null);
	function ask(req: ConfirmRequest, label: string, invalidate: readonly (readonly unknown[])[]) {
		setConfirm(req);
		setOnConfirmClose(() => done(label, invalidate));
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
	const violationPkgIds = Array.from(new Set(violations.map((v) => v.pkg_id)));
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
			<div className="hgrid sc" data-hgrid>
				{/* ── 1. Violations ── */}
				<section className="panel" {...panelProps('violations')} aria-label="Violations">
					<h3>
						<AlertTriangle className="h-3.5 w-3.5" />
						<span>Violations</span>
						{violationCount !== null && violationCount > 0 && (
							<span className="n bad" data-violn>
								{violationCount}
							</span>
						)}
						<span className="n warn" data-unsignedn>
							<Shield className="inline h-3 w-3" />{' '}
							{snapshotReady && trustDown.length === 0 ? unsigned.length : '—'} unsigned
						</span>
					</h3>

					<div className="hsub">
						<span>Permission violations</span>
						<span className="rt">
							<button
								type="button"
								className="chip"
								aria-label="Refresh violations"
								disabled={violationsQ.isFetching}
								onClick={() => void violationsQ.refetch()}
							>
								<RefreshCw className="h-3 w-3" />
							</button>
						</span>
					</div>
					{violationsQ.isLoading ? (
						<div className="hnote">Loading violations…</div>
					) : violationsQ.error ? (
						<Err>Failed to load violations: {errText(violationsQ.error)}</Err>
					) : violations.length === 0 ? (
						<div className="hnote" data-empty="violations">
							No violations recorded.
						</div>
					) : (
						<>
							<div className="hnote">
								{violationPkgIds.map((id) => (
									<button
										key={id}
										type="button"
										className="chip clear"
										data-clear={id}
										onClick={() => {
											const n = violations.filter((v) => v.pkg_id === id).length;
											ask(
												{
													title: `Clear violations for ${id}`,
													confirmLabel: 'Clear',
													body: (
														<>
															<p>
																Deletes the {n} audit row{n === 1 ? '' : 's'} for <code>{id}</code> from{' '}
																<code>pkg_permission_violations</code>.
															</p>
															<p>Audit-only: trust state and grants are unchanged. There is no undo.</p>
														</>
													),
													run: () => pkgPermissionViolationsClear(id),
												},
												`Clear violations for ${id}`,
												[HEALTH_KEYS.violations]
											);
										}}
									>
										Clear {id}
									</button>
								))}
							</div>
							<div className="hlist" data-list="violations">
								{violations.map((v) => (
									<div key={v.id} className="hrow" data-violation={v.id}>
										<AlertTriangle className="h-4 w-4 flex-none" />
										<div className="txt">
											<span className="t1">
												{v.pkg_id} <span className="tag">{violationScopeLabel(v.scope_kind)}</span>
											</span>
											<span className="t2">
												attempted <code>{v.attempted}</code> · declared <code>{v.declared || '—'}</code> ·{' '}
												{fmtTime(v.occurred_at)}
											</span>
										</div>
									</div>
								))}
							</div>
							<div className="hnote">
								Showing {violations.length} most recent (cap 200). Local audit data only.
							</div>
						</>
					)}

					<div className="hsub">
						<span>Install records</span>
						<span className="rt">
							{installs.length > 0 && (
								<button
									type="button"
									className="chip danger"
									data-act="remove-all"
									onClick={() =>
										ask(
											{
												title: `Remove all ${installs.length} unhealthy records`,
												confirmLabel: 'Remove all',
												body: (
													<>
														<p>
															Deletes every detected record: each broken <code>pkg_installed</code> row with its
															child <code>pkg_*</code> rows, and each orphan row.
														</p>
														<p>{installs.map((r) => r.id).join(', ')}</p>
														<p>Files on disk are never touched. There is no undo.</p>
													</>
												),
												run: () => pkgHealthRemoveAll(),
											},
											'Remove all records',
											[HEALTH_KEYS.installs, ngwaSnapshotQueryKey]
										)
									}
								>
									Remove all
								</button>
							)}
							<button
								type="button"
								className="chip"
								aria-label="Rescan install records"
								disabled={installsQ.isFetching}
								onClick={() => void installsQ.refetch()}
							>
								<RefreshCw className="h-3 w-3" />
							</button>
						</span>
					</div>
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
							{installs.map((r) => (
								<div key={`${r.id}:${r.issue.kind}`} className="hrow" data-install={r.id}>
									<AlertTriangle className="h-4 w-4 flex-none" />
									<div className="txt">
										<span className="t1">
											{r.id}{' '}
											<span
												className={`tag ${r.issue.kind === 'orphan_row' ? '' : 'bad'}`}
												data-issue={r.issue.kind}
											>
												{issueLabel(r.issue)}
											</span>
											{!r.enabled && <span className="tag">disabled</span>}
										</span>
										<span className="t2">
											{r.install_path ? <code>{r.install_path}</code> : '—'} · {r.detail}
										</span>
									</div>
									<div className="acts">
										<button
											type="button"
											className="chip danger"
											data-remove={r.id}
											onClick={() =>
												ask(
													{
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
																	: its <code>pkg_installed</code> row and child <code>pkg_*</code> rows, or
																	the orphan row.
																</p>
																<p>Files on disk are never touched. There is no undo.</p>
															</>
														),
														run: () => pkgHealthRemove(r.id),
													},
													`Remove record ${r.id}`,
													[HEALTH_KEYS.installs, ngwaSnapshotQueryKey]
												)
											}
										>
											Remove
										</button>
									</div>
								</div>
							))}
						</div>
					)}

					<div className="hrow">
						<Shield className="h-4 w-4 flex-none" />
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
										Unsigned means no signature and no approval pending. Skills, agents, commands and
										hooks carry no manifest, so they can never be signed; pkgs are unsigned when their
										manifest has no signature.
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

				{/* ── 5. Engines ── */}
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
												: 'Engine pkgs unknown: the snapshot is unavailable.'
											: pkgItem
												? `engine pkg ${pkgItem.name} ${pkgItem.version ?? '—'} · ${placed} items placed`
												: `No engine pkg installed, so Scopes shows no ${eng} column · ${placed} items placed`}
									</span>
									<span className="t2" data-engine-probe>
										{probe?.isLoading
											? 'Probing the CLI…'
											: probe?.error
												? `CLI probe failed: ${errText(probe.error)}`
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
