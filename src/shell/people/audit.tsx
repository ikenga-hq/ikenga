// Audit tab — D-05 `audit` (`designs/people.html` §7, `people-audit.png`),
// WP-77 (G-ACCESS §6, DEC-80).
//
// - The log: Who · Device · Action · Target · When, newest first, from
//   `access_audit_list` (§9.1), with "Load older" paging by `seq`.
// - The filter row (D-05 `.chiprow`): Who, Device and Kind (= category)
//   chips with counts, a search box and Export — applied to the loaded rows
//   the way D-05 does; Export sends the same filter to `access_audit_export`.
// - The scope switch: both scopes are live (§11.1): personal = rows you
//   acted in or are about, project = the active project's rows.
// - Export (§6.8): the desktop picks a file (`destPath`, the operator bearer
//   only); a browser downloads `{jsonl}`. Default name
//   `ikenga-audit-<YYYY-MM-DD>.jsonl`.
// - Degraded (§6.4): the banner "The audit chain is broken at #<seq> —
//   access changes are paused. An operator must reseal it." The desktop
//   (T0 operator) can reseal here; on T1 it is the root CLI.
// - §6.7 / P-14: below `full` (no `settings`) the tab says why instead.
// - The append-only rule box, then the D-4 file bar (no "Open file", no
//   iyke line — D-4, D-10).
//
// The daemon / broker decides what is visible; this view only presents.

import { Download, RefreshCw, Search, ShieldAlert } from 'lucide-react';
import { useCallback, useEffect, useMemo, useState } from 'react';

import { Button } from '@/components/ui/button';
import { FloatingToastChip } from '@/components/ui/floating-toast-chip';
import { cn } from '@/components/ui/utils';
import {
	type AccessStatus,
	type AuditFilter,
	accessAuditExport,
	accessAuditList,
	accessAuditReseal,
	accessStatus,
	parseAccessError,
} from '@/lib/access/client';
import { isTauri } from '@/lib/transport';
import { confirm as confirmDialog, save as saveDialog } from '@/lib/transport/dialog-shim';

import {
	AUDIT_CATEGORIES,
	AUDIT_RULE,
	type AuditPage,
	type AuditRow,
	type AuditScope,
	actionLabel,
	auditReadReason,
	brokenBanner,
	type ChipFilter,
	type ChipOption,
	canReseal,
	categoryCounts,
	deviceLabel,
	deviceOptions,
	exportFileName,
	exportFilter,
	matchesChips,
	NO_CHIPS,
	scopeFilter,
	targetLabel,
	whenLabel,
	whoLabel,
	whoOptions,
} from './audit-model';
import { D05_FOCUS } from './focus';
import { Kv, PeopleFileBar, PeopleHeader } from './frame';
import { tierOf, useTabProject } from './members';

const PAGE = 200;

const UNAVAILABLE =
	"The audit log lives in the background server's access store, and it isn't available right now (no ikenga-server, or it runs without a data folder).";

function useStatus() {
	const [status, setStatus] = useState<AccessStatus | null>(null);
	const [loaded, setLoaded] = useState(false);
	const reload = useCallback(async () => {
		try {
			setStatus(await accessStatus());
		} catch {
			setStatus(null);
		} finally {
			setLoaded(true);
		}
	}, []);
	useEffect(() => {
		void reload();
	}, [reload]);
	return { status, loaded, reload };
}

function useAuditRows(filter: AuditFilter, enabled: boolean) {
	const [rows, setRows] = useState<AuditRow[]>([]);
	const [nextBefore, setNextBefore] = useState<number | null>(null);
	const [error, setError] = useState<string | null>(null);
	const [loading, setLoading] = useState(false);
	const key = JSON.stringify(filter);
	// biome-ignore lint/correctness/useExhaustiveDependencies: `key` is `filter`'s value identity
	const reload = useCallback(async () => {
		if (!enabled) return;
		setLoading(true);
		try {
			const page = (await accessAuditList(filter, undefined, PAGE)) as AuditPage;
			setRows(page.rows);
			setNextBefore(page.nextBefore);
			setError(null);
		} catch (e) {
			const { code, message } = parseAccessError(e);
			setRows([]);
			setNextBefore(null);
			setError(code === 'store_unavailable' ? UNAVAILABLE : message);
		} finally {
			setLoading(false);
		}
	}, [key, enabled]);
	// biome-ignore lint/correctness/useExhaustiveDependencies: `key` is `filter`'s value identity
	const loadOlder = useCallback(async () => {
		if (!enabled || nextBefore === null) return;
		setLoading(true);
		try {
			const page = (await accessAuditList(filter, nextBefore, PAGE)) as AuditPage;
			setRows((prev) => [...prev, ...page.rows]);
			setNextBefore(page.nextBefore);
		} catch (e) {
			setError(parseAccessError(e).message);
		} finally {
			setLoading(false);
		}
	}, [key, enabled, nextBefore]);
	useEffect(() => {
		void reload();
	}, [reload]);
	return { rows, nextBefore, error, loading, reload, loadOlder };
}

/** Browser download of an export (§6.8: the broker never writes a file). */
function download(jsonl: string, name: string) {
	const url = URL.createObjectURL(new Blob([jsonl], { type: 'application/x-ndjson' }));
	const a = document.createElement('a');
	a.href = url;
	a.download = name;
	document.body.appendChild(a);
	a.click();
	a.remove();
	setTimeout(() => URL.revokeObjectURL(url), 1_000);
}

export function AuditTab() {
	const { status, loaded, reload: reloadStatus } = useStatus();
	const { projectId, projectName } = useTabProject();
	const tier = tierOf(status);
	const [scope, setScope] = useState<AuditScope>('personal');
	const readReason = auditReadReason(status);
	const base = useMemo(() => scopeFilter(scope, status, projectId), [scope, status, projectId]);
	const { rows, nextBefore, error, loading, reload, loadOlder } = useAuditRows(
		base,
		loaded && readReason === null
	);
	const [chips, setChips] = useState<ChipFilter>(NO_CHIPS);
	const [toast, setToast] = useState<{ label: string; error?: boolean } | null>(null);
	const [busy, setBusy] = useState(false);
	const visible = useMemo(() => rows.filter((r) => matchesChips(r, chips)), [rows, chips]);
	const brokenAt = status?.store === 'degraded' ? (status.brokenAtSeq ?? null) : null;

	const mode = readReason
		? 'forbidden'
		: error
			? 'unavailable'
			: !loaded || (loading && rows.length === 0)
				? 'loading'
				: rows.length === 0
					? 'empty'
					: 'rows';

	const refresh = useCallback(async () => {
		await Promise.all([reloadStatus(), reload()]);
	}, [reloadStatus, reload]);

	const exportLog = useCallback(async () => {
		const filter = exportFilter(base, chips);
		const name = exportFileName(Date.now());
		setBusy(true);
		try {
			// §6.8: only the desktop (the T0 operator bearer) names a path.
			if (isTauri() && canReseal(status)) {
				const path = await saveDialog({
					title: 'Export the audit log',
					defaultPath: name,
					filters: [{ name: 'JSON Lines', extensions: ['jsonl'] }],
				});
				if (!path) return;
				const r = await accessAuditExport(filter, path);
				const where = 'path' in r ? r.path : path;
				// The rows the file holds, as the daemon counted them (review
				// m-7) — never the rows this view happens to show.
				const n = 'rows' in r && typeof r.rows === 'number' ? r.rows : null;
				setToast({
					label:
						n === null ? `Exported the audit log to ${where}` : `Exported ${n} rows to ${where}`,
				});
			} else {
				const r = await accessAuditExport(filter);
				if ('jsonl' in r) {
					download(r.jsonl, name);
					const n = Math.max(0, r.jsonl.split('\n').filter(Boolean).length - 1);
					setToast({
						label: `Exported ${n} rows to ${name}${r.truncated ? ' (the newest 50 000)' : ''}`,
					});
				}
			}
			void reload();
		} catch (e) {
			setToast({ label: `Export failed: ${parseAccessError(e).message}`, error: true });
		} finally {
			setBusy(false);
		}
	}, [base, chips, status, reload]);

	const reseal = useCallback(async () => {
		if (brokenAt === null) return;
		const ok = await confirmDialog(
			`Reseal the audit chain at #${brokenAt}? The break stays in the log for good; access changes resume.`,
			{ title: 'Reseal the audit chain', kind: 'warning', okLabel: 'Reseal' }
		);
		if (!ok) return;
		setBusy(true);
		try {
			await accessAuditReseal(brokenAt);
			setToast({ label: `Resealed at #${brokenAt}` });
			await refresh();
		} catch (e) {
			setToast({ label: `Reseal failed: ${parseAccessError(e).message}`, error: true });
		} finally {
			setBusy(false);
		}
	}, [brokenAt, refresh]);

	return (
		<div
			data-state="audit"
			data-audit={mode}
			data-store={brokenAt === null ? 'ok' : 'degraded'}
			className={`${D05_FOCUS} mx-auto w-full max-w-[960px] space-y-4 px-6 py-6`}
		>
			<PeopleHeader
				tab="audit"
				scope={scope}
				onScope={setScope}
				auditWhy={loaded ? readReason : null}
			/>
			{brokenAt !== null && (
				<BrokenBanner seq={brokenAt} status={status} onReseal={() => void reseal()} busy={busy} />
			)}
			{readReason ? (
				<p
					data-audit-reason
					className="m-0 rounded-[var(--radius-md,6px)] border border-[var(--border)] bg-[var(--bg-surface)] px-3 py-3 text-[length:var(--text-caption,12px)] text-[var(--fg-muted)]"
				>
					{readReason}
				</p>
			) : (
				<section
					aria-label="Audit log"
					className="overflow-hidden rounded-[var(--radius-md,6px)] border border-[var(--border)] bg-[var(--bg-surface)]"
				>
					<Filters
						rows={rows}
						chips={chips}
						onChips={setChips}
						onExport={() => void exportLog()}
						exportDisabled={busy || error !== null}
						onRefresh={() => void refresh()}
						loading={loading}
						scopeNote={
							scope === 'project' ? `${projectName}` : tier === 't1' ? 'you' : 'this computer'
						}
					/>
					<AuditTable rows={visible} total={rows.length} mode={mode} error={error} />
					{nextBefore !== null && (
						<div className="flex justify-center border-t border-[var(--border-soft)] py-2">
							<Button
								type="button"
								variant="ghost"
								size="xs"
								disabled={loading}
								onClick={() => void loadOlder()}
							>
								Load older
							</Button>
						</div>
					)}
				</section>
			)}
			<p className="m-0 rounded-[var(--radius-md,6px)] border border-dashed border-[var(--border)] px-3 py-2.5 text-[length:var(--text-caption,12px)] text-[var(--fg-muted)]">
				{AUDIT_RULE}
			</p>
			<PeopleFileBar t1={tier === 't1'} />
			{toast && (
				<div data-audit-toast>
					<FloatingToastChip
						variant={toast.error ? 'error' : 'info'}
						anchor="viewport-bottom-right"
						label={toast.label}
						ttlMs={6_000}
						onDismiss={() => setToast(null)}
					/>
				</div>
			)}
		</div>
	);
}

function BrokenBanner({
	seq,
	status,
	onReseal,
	busy,
}: {
	seq: number;
	status: AccessStatus | null;
	onReseal: () => void;
	busy: boolean;
}) {
	const how = canReseal(status)
		? null
		: status?.tier === 't1'
			? `On the server, as root: ikenga-server audit reseal --ack ${seq}`
			: 'Reseal it from the desktop on the host.';
	return (
		<div
			role="alert"
			data-audit-banner="degraded"
			className="flex flex-wrap items-center gap-2 rounded-[var(--radius-md,6px)] border border-[var(--danger)] px-3 py-2.5 text-[length:var(--text-caption,12px)] text-[var(--fg)]"
			style={{ background: 'color-mix(in srgb, var(--danger) 8%, transparent)' }}
		>
			<ShieldAlert className="h-4 w-4 flex-none text-[var(--danger)]" aria-hidden />
			<span className="min-w-0 flex-1">
				{brokenBanner(seq)}
				{how && (
					<span className="mt-0.5 block font-mono text-[length:var(--text-micro)] text-[var(--fg-muted)]">
						{how}
					</span>
				)}
			</span>
			{canReseal(status) && (
				<Button type="button" variant="outline" size="xs" disabled={busy} onClick={onReseal}>
					Reseal…
				</Button>
			)}
		</div>
	);
}

function Chip({
	on,
	label,
	count,
	onClick,
}: {
	on: boolean;
	label: string;
	count?: number;
	onClick: () => void;
}) {
	return (
		<button
			type="button"
			aria-pressed={on}
			onClick={onClick}
			className={cn(
				'inline-flex h-[22px] items-center gap-[5px] whitespace-nowrap rounded-full border px-2 text-[length:var(--text-micro)]',
				on
					? 'border-[var(--primary)] bg-[var(--primary-soft)] text-[var(--fg)]'
					: 'border-[var(--border)] bg-[var(--bg-surface)] text-[var(--fg-muted)] hover:border-[var(--border-strong)] hover:bg-[var(--bg-raised)] hover:text-[var(--fg)]'
			)}
		>
			{label}
			{count !== undefined && (
				<span className="font-mono text-[10px] text-[var(--fg-muted)]">{count}</span>
			)}
		</button>
	);
}

function ChipGroup({
	label,
	options,
	value,
	allCount,
	onPick,
}: {
	label: string;
	options: ReadonlyArray<Pick<ChipOption, 'id' | 'label'> & { count?: number }>;
	value: string;
	allCount?: number;
	onPick: (id: string) => void;
}) {
	return (
		// biome-ignore lint/a11y/useSemanticElements: a chip group, not a form fieldset
		<span role="group" aria-label={label} className="contents">
			<span className="text-[length:var(--text-micro)] font-semibold uppercase tracking-[0.1em] text-[var(--fg-muted)]">
				{label}
			</span>
			<Chip on={value === 'all'} label="All" count={allCount} onClick={() => onPick('all')} />
			{options.map((o) => (
				<Chip
					key={o.id}
					on={value === o.id}
					label={o.label}
					count={o.count}
					onClick={() => onPick(o.id)}
				/>
			))}
		</span>
	);
}

function Filters({
	rows,
	chips,
	onChips,
	onExport,
	exportDisabled,
	onRefresh,
	loading,
	scopeNote,
}: {
	rows: AuditRow[];
	chips: ChipFilter;
	onChips: (c: ChipFilter) => void;
	onExport: () => void;
	exportDisabled: boolean;
	onRefresh: () => void;
	loading: boolean;
	scopeNote: string;
}) {
	const counts = categoryCounts(rows);
	return (
		<div
			id="auditFilters"
			className="flex flex-wrap items-center gap-2 border-b border-[var(--border-soft)] px-3 py-2"
		>
			<ChipGroup
				label="Who"
				options={whoOptions(rows)}
				value={chips.who}
				allCount={rows.length}
				onPick={(who) => onChips({ ...chips, who })}
			/>
			<ChipGroup
				label="Device"
				// D-05 draws the Device chips without counts.
				options={deviceOptions(rows).map(({ id, label }) => ({ id, label }))}
				value={chips.device}
				onPick={(device) => onChips({ ...chips, device })}
			/>
			<ChipGroup
				label="Kind"
				options={AUDIT_CATEGORIES.map((c) => ({ id: c.id, label: c.label, count: counts[c.id] }))}
				value={chips.kind}
				onPick={(kind) => onChips({ ...chips, kind: kind as ChipFilter['kind'] })}
			/>
			<span className="flex h-6 min-w-[140px] flex-1 items-center gap-2 rounded-[var(--radius-sm,4px)] border border-[var(--border)] bg-[var(--bg-sunken)] px-2 text-[var(--fg-muted)]">
				<Search className="h-3 w-3 flex-none" aria-hidden />
				<input
					id="auditQ"
					type="text"
					placeholder="Search the log"
					aria-label="Search the audit log"
					value={chips.q}
					onChange={(e) => onChips({ ...chips, q: e.target.value })}
					className="min-w-0 flex-1 bg-transparent text-[length:var(--text-caption,12px)] text-[var(--fg)] outline-none placeholder:text-[var(--fg-muted)]"
				/>
			</span>
			<Button
				id="auditExport"
				type="button"
				variant="outline"
				size="xs"
				disabled={exportDisabled}
				onClick={onExport}
			>
				<Download /> Export
			</Button>
			<span className="ml-auto flex items-center gap-1">
				<Kv>{scopeNote}</Kv>
				<Button
					type="button"
					variant="ghost"
					size="icon-xs"
					aria-label="Refresh the log"
					title="Refresh the log"
					disabled={loading}
					onClick={onRefresh}
				>
					<RefreshCw className={loading ? 'animate-spin' : undefined} />
				</Button>
			</span>
		</div>
	);
}

function AuditTable({
	rows,
	total,
	mode,
	error,
}: {
	rows: AuditRow[];
	total: number;
	mode: string;
	error: string | null;
}) {
	const now = Date.now();
	const empty =
		mode === 'unavailable'
			? (error ?? UNAVAILABLE)
			: mode === 'loading'
				? 'Loading the log…'
				: total === 0
					? 'Nothing recorded yet.'
					: 'Nothing matches. Clear a filter.';
	return (
		<div className="overflow-x-auto">
			<table
				id="auditTable"
				className="w-full border-collapse text-left text-[length:var(--text-caption,12px)]"
			>
				<thead>
					<tr className="border-b border-[var(--border-soft)] text-[length:var(--text-micro)] uppercase tracking-[0.08em] text-[var(--fg-muted)]">
						<th className="px-3 py-2 font-semibold">Who</th>
						<th className="px-3 py-2 font-semibold">Device</th>
						<th className="px-3 py-2 font-semibold">Action</th>
						<th className="px-3 py-2 font-semibold">Target</th>
						<th className="px-3 py-2 text-right font-semibold">When</th>
					</tr>
				</thead>
				<tbody id="auditBody">
					{rows.length === 0 ? (
						<tr>
							<td colSpan={5} className="px-3 py-4 text-[var(--fg-muted)]">
								{empty}
							</td>
						</tr>
					) : (
						rows.map((r) => (
							<tr
								key={r.seq}
								data-seq={r.seq}
								data-kind={r.kind}
								data-category={r.category}
								className="border-b border-[var(--border-soft)] last:border-b-0"
							>
								<td className="px-3 py-2 text-[var(--fg)]">{whoLabel(r)}</td>
								<td className="px-3 py-2 font-mono text-[length:var(--text-micro)] text-[var(--fg-muted)]">
									{deviceLabel(r)}
								</td>
								<td className="px-3 py-2 text-[var(--fg)]">{actionLabel(r)}</td>
								<td className="max-w-[280px] truncate px-3 py-2 font-mono text-[length:var(--text-micro)] text-[var(--fg)]">
									{targetLabel(r)}
								</td>
								<td
									className="px-3 py-2 text-right font-mono text-[var(--fg-muted)]"
									title={new Date(r.atMs).toISOString()}
								>
									{whenLabel(r.atMs, now)}
								</td>
							</tr>
						))
					)}
				</tbody>
			</table>
		</div>
	);
}
