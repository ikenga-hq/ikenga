// The admin "Server" card's data and its judgement.
//
// `ServerHealth` mirrors `src-tauri/src/server/host_health.rs` (`HostHealth`)
// field for field; every section is optional because the server omits what
// it cannot measure (`unavailable` names them). `assess()` turns a snapshot
// into the chips the card shows, with the thresholds in one place.
//
// Thresholds (founder brief 2026-10-08, from a 2 vCPU / 3.8 GB / no-swap box):
//   warning  memory available < 15 %
//   warning  no swap while memory is tight (available < 25 %)
//   warning  disk free < 10 %           (error below 5 %)
//   warning  memory pressure (PSI some avg60) > 10
//   warning  a backup older than twice its schedule's interval
//   error    a database whose last backup attempt failed
//   error    a failed backup run, backup timer or tunnel
//   warning  a backup timer or tunnel that is not running

export interface Load {
	m1: number;
	m5: number;
	m15: number;
}
export interface Memory {
	total_bytes: number;
	available_bytes: number;
}
export interface Swap {
	total_bytes: number;
	used_bytes: number;
}
export interface Psi {
	some_avg10: number;
	some_avg60: number;
	full_avg10?: number;
	full_avg60?: number;
}
export interface Pressure {
	cpu?: Psi;
	memory?: Psi;
	io?: Psi;
}
export interface Disk {
	total_bytes: number;
	free_bytes: number;
}
export interface BackupDb {
	name: string;
	schedule: string | null;
	last_attempt: string | null;
	last_attempt_ok: boolean | null;
	last_success: string | null;
	last_error_kind: string | null;
	last_error_at: string | null;
	bytes: number | null;
	duration_s: number | null;
}
export interface BackupSchedule {
	name: string;
	last_run: string | null;
	last_run_ok: boolean | null;
	failed: string[];
}
export interface Backups {
	enabled: boolean;
	updated: string | null;
	databases: BackupDb[];
	schedules: BackupSchedule[];
}
export interface UnitState {
	name: string;
	kind: 'timer' | 'tunnel' | 'backup_run';
	active_state: string;
	sub_state: string | null;
	result: string | null;
	last_trigger: number | null;
	next_elapse: number | null;
}
export interface AccountLoad {
	username: string;
	running: boolean;
	terminals?: number;
	claude_processes?: number;
}
export interface ServerHealth {
	schema: number;
	taken_at_ms: number;
	version: string;
	tier: 't0' | 't1';
	cpu_count?: number;
	load?: Load;
	memory?: Memory;
	swap?: Swap;
	pressure?: Pressure;
	disk?: Disk;
	uptime_secs?: number;
	backups?: Backups;
	units?: UnitState[];
	accounts?: AccountLoad[];
	unavailable: string[];
	/** Set when the server answered with its last good snapshot because a
	 *  fresh one overran. */
	stale?: boolean;
}

/** The snapshot shape this client understands. */
export const SUPPORTED_SCHEMA = 1;

export type Level = 'ok' | 'warn' | 'error' | 'unknown';

export interface Chip {
	id: string;
	label: string;
	/** The plain number(s). */
	value: string;
	level: Level;
	/** Why it is not green. Empty when it is. */
	note?: string;
}

export interface Assessment {
	chips: Chip[];
	overall: Level;
}

// ─── thresholds ─────────────────────────────────────────────────────────────

export const MEMORY_WARN_AVAILABLE = 0.15;
export const MEMORY_TIGHT_NO_SWAP = 0.25;
export const DISK_WARN_FREE = 0.1;
export const DISK_ERROR_FREE = 0.05;
export const PSI_MEMORY_WARN_AVG60 = 10;
/** Load per core, 5-minute average. */
export const LOAD_WARN_PER_CORE = 1.5;

/** The backup job's schedules and how old a success may get before it is
 *  stale: twice the interval (scripts/server/README.md "alert contract"). */
const STALE_AFTER_HOURS: Record<string, number> = {
	'4hourly': 9,
	daily: 26,
	weekly: 8 * 24,
	monthly: 32 * 24,
};

const worst = (a: Level, b: Level): Level => (RANK[b] > RANK[a] ? b : a);
const RANK: Record<Level, number> = { unknown: 0, ok: 1, warn: 2, error: 3 };

// ─── formatting ─────────────────────────────────────────────────────────────

export function fmtBytes(n: number): string {
	if (!Number.isFinite(n) || n < 0) return '—';
	const units = ['B', 'KB', 'MB', 'GB', 'TB'];
	let v = n;
	let i = 0;
	while (v >= 1024 && i < units.length - 1) {
		v /= 1024;
		i++;
	}
	return `${i === 0 || v >= 100 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

export function fmtUptime(secs: number): string {
	const d = Math.floor(secs / 86_400);
	const h = Math.floor((secs % 86_400) / 3_600);
	const m = Math.floor((secs % 3_600) / 60);
	if (d > 0) return `${d}d ${h}h`;
	if (h > 0) return `${h}h ${m}m`;
	return `${m}m`;
}

const pct = (part: number, whole: number) => (whole > 0 ? (part / whole) * 100 : 0);

// ─── assessment ─────────────────────────────────────────────────────────────

function memoryChip(h: ServerHealth): Chip {
	const m = h.memory;
	if (!m) return { id: 'memory', label: 'Memory', value: '—', level: 'unknown' };
	const availFrac = m.total_bytes > 0 ? m.available_bytes / m.total_bytes : 1;
	const noSwap = h.swap !== undefined && h.swap.total_bytes === 0;
	let level: Level = 'ok';
	let note: string | undefined;
	if (availFrac < MEMORY_WARN_AVAILABLE) {
		level = 'warn';
		note = `Only ${Math.round(availFrac * 100)}% of memory is available.${noSwap ? ' There is no swap to fall back on.' : ''}`;
	} else if (noSwap && availFrac < MEMORY_TIGHT_NO_SWAP) {
		level = 'warn';
		note = 'Memory is getting tight and there is no swap to fall back on.';
	}
	return {
		id: 'memory',
		label: 'Memory',
		value: `${fmtBytes(m.available_bytes)} free of ${fmtBytes(m.total_bytes)}`,
		level,
		note,
	};
}

function swapChip(h: ServerHealth): Chip | null {
	const s = h.swap;
	if (!s) return null;
	return {
		id: 'swap',
		label: 'Swap',
		value:
			s.total_bytes === 0 ? 'none' : `${fmtBytes(s.used_bytes)} of ${fmtBytes(s.total_bytes)} used`,
		// Its absence is judged with memory above; on its own it is a fact.
		level: 'ok',
	};
}

function diskChip(h: ServerHealth): Chip {
	const d = h.disk;
	if (!d) return { id: 'disk', label: 'Disk', value: '—', level: 'unknown' };
	const freeFrac = d.total_bytes > 0 ? d.free_bytes / d.total_bytes : 1;
	let level: Level = 'ok';
	let note: string | undefined;
	if (freeFrac < DISK_ERROR_FREE) {
		level = 'error';
		note = `Under ${Math.round(DISK_ERROR_FREE * 100)}% of the disk is free.`;
	} else if (freeFrac < DISK_WARN_FREE) {
		level = 'warn';
		note = `Under ${Math.round(DISK_WARN_FREE * 100)}% of the disk is free.`;
	}
	return {
		id: 'disk',
		label: 'Disk',
		value: `${fmtBytes(d.free_bytes)} free of ${fmtBytes(d.total_bytes)}`,
		level,
		note,
	};
}

function pressureChip(h: ServerHealth): Chip | null {
	const p = h.pressure;
	if (!p) return null;
	const mem = p.memory;
	const parts = [
		p.cpu && `cpu ${p.cpu.some_avg60.toFixed(1)}`,
		mem && `memory ${mem.some_avg60.toFixed(1)}`,
		p.io && `io ${p.io.some_avg60.toFixed(1)}`,
	].filter(Boolean);
	const warn = mem !== undefined && mem.some_avg60 > PSI_MEMORY_WARN_AVG60;
	return {
		id: 'pressure',
		label: 'Pressure (60 s)',
		value: parts.join(' · ') || '—',
		level: warn ? 'warn' : 'ok',
		note: warn ? 'Tasks have been waiting on memory. The box is swapping or about to.' : undefined,
	};
}

function loadChip(h: ServerHealth): Chip | null {
	const l = h.load;
	if (!l) return null;
	const cores = h.cpu_count ?? 1;
	const warn = l.m5 > LOAD_WARN_PER_CORE * cores;
	return {
		id: 'load',
		label: 'Load',
		value: `${l.m1.toFixed(2)} · ${l.m5.toFixed(2)} · ${l.m15.toFixed(2)}${h.cpu_count ? ` on ${h.cpu_count} cores` : ''}`,
		level: warn ? 'warn' : 'ok',
		note: warn ? 'More work is queued than the cores can run.' : undefined,
	};
}

function backupsChip(h: ServerHealth, nowMs: number): Chip {
	const b = h.backups;
	if (!b) {
		return {
			id: 'backups',
			label: 'Backups',
			value: 'not configured',
			level: 'unknown',
		};
	}
	if (!b.enabled) {
		return {
			id: 'backups',
			label: 'Backups',
			value: 'disabled',
			level: 'warn',
			note: 'Backups are switched off on this server.',
		};
	}
	const failing = b.databases.filter((d) => d.last_error_kind || d.last_attempt_ok === false);
	if (failing.length > 0) {
		return {
			id: 'backups',
			label: 'Backups',
			value: `${failing.length} of ${b.databases.length} failing`,
			level: 'error',
			note: failing.map((d) => `${d.name}: ${d.last_error_kind ?? 'failed'}`).join(' · '),
		};
	}
	const stale = b.databases.filter((d) => isStale(d, nowMs));
	if (stale.length > 0) {
		return {
			id: 'backups',
			label: 'Backups',
			value: `${stale.length} of ${b.databases.length} overdue`,
			level: 'warn',
			note: stale.map((d) => d.name).join(' · '),
		};
	}
	return {
		id: 'backups',
		label: 'Backups',
		value: `${b.databases.length} database${b.databases.length === 1 ? '' : 's'}, all current`,
		level: 'ok',
	};
}

/** Whether a database's last success is older than twice its interval, or it
 *  has never succeeded. Unknown schedules are not judged. */
export function isStale(d: BackupDb, nowMs: number): boolean {
	const hours = d.schedule ? STALE_AFTER_HOURS[d.schedule] : undefined;
	if (hours === undefined) return false;
	if (!d.last_success) return true;
	const t = Date.parse(d.last_success);
	return Number.isFinite(t) && nowMs - t > hours * 3_600_000;
}

function unitsChip(h: ServerHealth): Chip | null {
	const units = h.units;
	if (!units) return null;
	if (units.length === 0) {
		return { id: 'units', label: 'Timers & tunnels', value: 'none installed', level: 'unknown' };
	}
	const failed = units.filter(unitFailed);
	if (failed.length > 0) {
		return {
			id: 'units',
			label: 'Timers & tunnels',
			value: `${failed.length} failed`,
			level: 'error',
			note: failed.map((u) => u.name).join(' · '),
		};
	}
	const idle = units.filter((u) => u.kind !== 'backup_run' && u.active_state !== 'active');
	if (idle.length > 0) {
		return {
			id: 'units',
			label: 'Timers & tunnels',
			value: `${idle.length} not running`,
			level: 'warn',
			note: idle.map((u) => u.name).join(' · '),
		};
	}
	return {
		id: 'units',
		label: 'Timers & tunnels',
		value: `${units.length} running`,
		level: 'ok',
	};
}

export function unitFailed(u: UnitState): boolean {
	return (
		u.active_state === 'failed' || (u.result !== null && !['success', '', 'n/a'].includes(u.result))
	);
}

/** The card's chips, worst first within their natural order. */
export function assess(h: ServerHealth, nowMs: number = Date.now()): Assessment {
	const chips = [
		memoryChip(h),
		swapChip(h),
		pressureChip(h),
		diskChip(h),
		loadChip(h),
		backupsChip(h, nowMs),
		unitsChip(h),
	].filter((c): c is Chip => c !== null);
	const overall = chips.reduce<Level>((acc, c) => worst(acc, c.level), 'ok');
	return { chips, overall };
}

/** Memory used as a percentage, for a bar. */
export function memoryUsedPct(m: Memory): number {
	return Math.round(pct(m.total_bytes - m.available_bytes, m.total_bytes));
}
