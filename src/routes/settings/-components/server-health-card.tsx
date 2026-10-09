// Settings › About › Server health (the admin "Server" card).
//
// Why here and not on the Ngwa Health page: that page is the *package*
// health dashboard (violations, sidecars, cron, engines) and works on the
// local machine's pkgs. This card describes the box the daemon runs on, and
// its natural neighbour already exists: the WP-P9 "Server updates" panel in
// Settings › About, which is also admin-only and browser-only. The Health
// page links here (see `ConnectionPanel`).
//
// Shown only where `useServerHealth()` is enabled and has a snapshot: a
// browser tab of a T1 admin or the T0 owner. Never on the desktop (the
// machine at hand needs no card) and never for a member.

import { Activity } from 'lucide-react';
import type { ReactNode } from 'react';

import { cn } from '@/components/ui/utils';
import { useServerHealth } from '@/lib/queries/server-health';
import {
	type Assessment,
	assess,
	type Chip,
	fmtBytes,
	fmtUptime,
	type Level,
	memoryUsedPct,
	type ServerHealth,
	unitFailed,
} from '@/lib/server-health/model';

import { SettingGroup } from './setting-group';

const LEVEL_COPY: Record<Level, string> = {
	ok: 'OK',
	warn: 'Warning',
	error: 'Error',
	unknown: 'Unknown',
};

const LEVEL_TEXT: Record<Level, string> = {
	ok: 'text-[var(--success)]',
	warn: 'text-[var(--warning)]',
	error: 'text-[var(--danger)]',
	unknown: 'text-muted-foreground',
};

const LEVEL_DOT: Record<Level, string> = {
	ok: 'bg-[var(--success)]',
	warn: 'bg-[var(--warning)]',
	error: 'bg-[var(--danger)]',
	unknown: 'bg-muted-foreground/50',
};

/** The panel, wired to the query. Renders nothing for anyone who may not see
 *  it, for an unreadable snapshot, and while the first read is in flight. */
export function ServerHealthPanel() {
	const { data, isFetching } = useServerHealth();
	// A refused or failed first read is not worth a card for a person who
	// never asked for one; a failure after a good read keeps the old card.
	if (!data) return null;
	return <ServerHealthCard health={data} refreshing={isFetching} />;
}

export function ServerHealthCard({
	health,
	refreshing = false,
	now = Date.now,
}: {
	health: ServerHealth;
	refreshing?: boolean;
	now?: () => number;
}) {
	const verdict = assess(health, now());
	return (
		<div id="server-health" data-testid="server-health-card">
			<SettingGroup title="Server health">
				<div className="space-y-3 px-4 py-3.5 text-sm">
					<Summary health={health} verdict={verdict} refreshing={refreshing} now={now()} />
					<ul className="grid gap-2 sm:grid-cols-2" aria-label="Server health checks">
						{verdict.chips.map((c) => (
							<ChipRow key={c.id} chip={c} />
						))}
					</ul>
					{health.memory && <MemoryBar used={memoryUsedPct(health.memory)} />}
				</div>
				<Details health={health} />
			</SettingGroup>
		</div>
	);
}

function Summary({
	health,
	verdict,
	refreshing,
	now,
}: {
	health: ServerHealth;
	verdict: Assessment;
	refreshing: boolean;
	now: number;
}) {
	const ageSecs = Math.max(0, Math.round((now - health.taken_at_ms) / 1000));
	return (
		<div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
			<span
				className={cn('inline-flex items-center gap-1.5 font-medium', LEVEL_TEXT[verdict.overall])}
				data-testid="server-health-overall"
				data-level={verdict.overall}
			>
				<Activity aria-hidden className="size-3.5" />
				{verdict.overall === 'ok' ? 'All checks pass' : LEVEL_COPY[verdict.overall]}
			</span>
			<span className="font-mono text-[11px] text-muted-foreground">
				v{health.version}
				{health.uptime_secs !== undefined && <> · up {fmtUptime(health.uptime_secs)}</>}
				{health.cpu_count !== undefined && <> · {health.cpu_count} cores</>}
			</span>
			<span className="font-mono text-[11px] text-muted-foreground/80" aria-live="off">
				{health.stale ? 'last good reading, ' : ''}
				{refreshing ? 'refreshing…' : `${ageSecs}s ago`}
			</span>
		</div>
	);
}

function ChipRow({ chip }: { chip: Chip }) {
	return (
		<li
			data-chip={chip.id}
			data-level={chip.level}
			className="rounded-md border border-[var(--border-soft)] bg-background px-3 py-2"
		>
			<div className="flex items-center gap-2">
				<span aria-hidden className={cn('size-2 shrink-0 rounded-full', LEVEL_DOT[chip.level])} />
				<span className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
					{chip.label}
				</span>
				<span className={cn('ml-auto text-[11px] font-medium', LEVEL_TEXT[chip.level])}>
					{LEVEL_COPY[chip.level]}
				</span>
			</div>
			<div className="mt-1 font-mono text-[12px]">{chip.value}</div>
			{chip.note && <p className="mt-1 text-xs text-muted-foreground">{chip.note}</p>}
		</li>
	);
}

function MemoryBar({ used }: { used: number }) {
	return (
		<div
			role="img"
			aria-label={`Memory ${used}% in use`}
			className="h-1.5 overflow-hidden rounded-full bg-[var(--bg-sunken)]"
		>
			<div
				className={cn('h-full', used >= 85 ? 'bg-[var(--warning)]' : 'bg-[var(--success)]')}
				style={{ width: `${Math.min(100, Math.max(0, used))}%` }}
			/>
		</div>
	);
}

function Details({ health }: { health: ServerHealth }) {
	const accounts = health.accounts ?? [];
	const units = health.units ?? [];
	const dbs = health.backups?.databases ?? [];
	if (accounts.length === 0 && units.length === 0 && dbs.length === 0 && !health.pressure) {
		return <Unavailable health={health} />;
	}
	return (
		<div className="space-y-3 px-4 py-3 text-sm">
			{health.pressure && (
				<Section title="Pressure (share of time tasks waited, 10 s / 60 s)">
					{(['cpu', 'memory', 'io'] as const).map((k) => {
						const p = health.pressure?.[k];
						return p ? (
							<Row
								key={k}
								name={k}
								value={`${p.some_avg10.toFixed(1)} / ${p.some_avg60.toFixed(1)}`}
							/>
						) : null;
					})}
				</Section>
			)}
			{dbs.length > 0 && (
				<Section title="Backups">
					{dbs.map((d) => (
						<Row
							key={d.name}
							name={d.name}
							level={d.last_error_kind || d.last_attempt_ok === false ? 'error' : 'ok'}
							value={
								d.last_error_kind
									? `failing: ${d.last_error_kind}`
									: `${d.last_success ? d.last_success.replace('T', ' ').replace('Z', ' UTC') : 'never'}${
											d.bytes != null ? ` · ${fmtBytes(d.bytes)}` : ''
										}`
							}
						/>
					))}
				</Section>
			)}
			{units.length > 0 && (
				<Section title="Timers and tunnels">
					{units.map((u) => (
						<Row
							key={u.name}
							name={u.name}
							level={unitFailed(u) ? 'error' : u.active_state === 'active' ? 'ok' : 'warn'}
							value={`${u.active_state}${u.sub_state && u.sub_state !== u.active_state ? ` (${u.sub_state})` : ''}${
								u.last_trigger
									? ` · last ${new Date(u.last_trigger * 1000).toISOString().slice(0, 16).replace('T', ' ')} UTC`
									: ''
							}`}
						/>
					))}
				</Section>
			)}
			{accounts.length > 0 && (
				<Section title="Accounts (counts only)">
					{accounts.map((a) => (
						<Row
							key={a.username}
							name={a.username}
							value={
								a.running
									? `${a.terminals ?? '—'} terminals · ${a.claude_processes ?? '—'} claude`
									: 'idle'
							}
						/>
					))}
				</Section>
			)}
			<Unavailable health={health} />
		</div>
	);
}

function Unavailable({ health }: { health: ServerHealth }) {
	if (health.unavailable.length === 0) return null;
	return (
		<p className="px-0 text-xs text-muted-foreground" data-testid="server-health-unavailable">
			Not measurable on this server: {health.unavailable.join(', ')}.
		</p>
	);
}

function Section({ title, children }: { title: string; children: ReactNode }) {
	return (
		<div>
			<h4 className="mb-1 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
				{title}
			</h4>
			<dl className="divide-y divide-[var(--border-soft)]">{children}</dl>
		</div>
	);
}

function Row({ name, value, level }: { name: string; value: string; level?: Level }) {
	return (
		<div className="flex items-baseline gap-3 py-1">
			<dt className="min-w-0 flex-1 truncate font-mono text-[12px]">{name}</dt>
			<dd
				className={cn('font-mono text-[12px]', level ? LEVEL_TEXT[level] : 'text-muted-foreground')}
			>
				{value}
			</dd>
		</div>
	);
}
