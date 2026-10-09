import { describe, expect, it } from 'vitest';
import {
	assess,
	fmtBytes,
	fmtUptime,
	isStale,
	type BackupDb,
	type Chip,
	type ServerHealth,
	type UnitState,
} from './model';

const GB = 1024 ** 3;
const NOW = Date.parse('2026-10-08T12:00:00Z');

/** The idle 2 vCPU / 3.8 GB / no-swap box of 2026-10-08, all green. */
function healthy(over: Partial<ServerHealth> = {}): ServerHealth {
	return {
		schema: 1,
		taken_at_ms: NOW,
		version: '0.22.0',
		tier: 't1',
		cpu_count: 2,
		load: { m1: 0.1, m5: 0.1, m15: 0.05 },
		memory: { total_bytes: 3.8 * GB, available_bytes: 2.9 * GB },
		swap: { total_bytes: 2 * GB, used_bytes: 0 },
		pressure: {
			cpu: { some_avg10: 0, some_avg60: 0.1 },
			memory: { some_avg10: 0, some_avg60: 0 },
			io: { some_avg10: 0, some_avg60: 0 },
		},
		disk: { total_bytes: 80 * GB, free_bytes: 50 * GB },
		uptime_secs: 86_400 * 3,
		backups: { enabled: true, updated: '2026-10-08T11:00:00Z', databases: [], schedules: [] },
		units: [],
		unavailable: [],
		...over,
	};
}

const db = (over: Partial<BackupDb> = {}): BackupDb => ({
	name: 'devotee',
	schedule: 'daily',
	last_attempt: '2026-10-08T03:00:00Z',
	last_attempt_ok: true,
	last_success: '2026-10-08T03:00:00Z',
	last_error_kind: null,
	last_error_at: null,
	bytes: 1000,
	duration_s: 3,
	...over,
});

const unit = (over: Partial<UnitState> = {}): UnitState => ({
	name: 'devotee-db-tunnel.service',
	kind: 'tunnel',
	active_state: 'active',
	sub_state: 'running',
	result: 'success',
	last_trigger: null,
	next_elapse: null,
	...over,
});

const chip = (h: ServerHealth, id: string): Chip => {
	const c = assess(h, NOW).chips.find((x) => x.id === id);
	if (!c) throw new Error(`no chip ${id}`);
	return c;
};

describe('assess: memory and swap', () => {
	it('is ok when plenty is available', () => {
		expect(chip(healthy(), 'memory').level).toBe('ok');
	});
	it('warns below 15 % available', () => {
		const h = healthy({ memory: { total_bytes: 100 * GB, available_bytes: 14 * GB } });
		expect(chip(h, 'memory').level).toBe('warn');
	});
	it('does not warn at exactly 15 %', () => {
		const h = healthy({ memory: { total_bytes: 100 * GB, available_bytes: 15 * GB } });
		expect(chip(h, 'memory').level).toBe('ok');
	});
	it('warns when there is no swap and memory is getting tight (under 25 %)', () => {
		const h = healthy({
			swap: { total_bytes: 0, used_bytes: 0 },
			memory: { total_bytes: 100 * GB, available_bytes: 20 * GB },
		});
		const c = chip(h, 'memory');
		expect(c.level).toBe('warn');
		expect(c.note).toMatch(/no swap/);
	});
	it('no swap alone is not a warning on a roomy box', () => {
		const h = healthy({ swap: { total_bytes: 0, used_bytes: 0 } });
		expect(chip(h, 'memory').level).toBe('ok');
		expect(chip(h, 'swap').value).toBe('none');
	});
	it('is unknown, not green, when memory could not be read', () => {
		const h = healthy({ memory: undefined, unavailable: ['memory'] });
		expect(chip(h, 'memory').level).toBe('unknown');
	});
});

describe('assess: disk', () => {
	it('warns under 10 % free, errors under 5 %', () => {
		const at = (free: number) =>
			healthy({ disk: { total_bytes: 100 * GB, free_bytes: free * GB } });
		expect(chip(at(11), 'disk').level).toBe('ok');
		expect(chip(at(9), 'disk').level).toBe('warn');
		expect(chip(at(4), 'disk').level).toBe('error');
	});
});

describe('assess: pressure', () => {
	it('warns when memory some avg60 is over 10', () => {
		const h = healthy({
			pressure: { memory: { some_avg10: 30, some_avg60: 10.5 } },
		});
		expect(chip(h, 'pressure').level).toBe('warn');
	});
	it('does not warn at 10 or for cpu/io alone', () => {
		expect(
			chip(healthy({ pressure: { memory: { some_avg10: 0, some_avg60: 10 } } }), 'pressure').level
		).toBe('ok');
		expect(
			chip(healthy({ pressure: { cpu: { some_avg10: 90, some_avg60: 90 } } }), 'pressure').level
		).toBe('ok');
	});
	it('has no chip when the kernel reports no PSI', () => {
		const ids = assess(healthy({ pressure: undefined }), NOW).chips.map((c) => c.id);
		expect(ids).not.toContain('pressure');
	});
});

describe('assess: backups', () => {
	it('is ok when every database is current', () => {
		const h = healthy({
			backups: { enabled: true, updated: null, databases: [db()], schedules: [] },
		});
		expect(chip(h, 'backups').level).toBe('ok');
	});
	it('errors when any database has a last_error_kind', () => {
		const h = healthy({
			backups: {
				enabled: true,
				updated: null,
				databases: [db(), db({ name: 'other', last_error_kind: 'gcs-auth' })],
				schedules: [],
			},
		});
		const c = chip(h, 'backups');
		expect(c.level).toBe('error');
		expect(c.value).toBe('1 of 2 failing');
		expect(c.note).toContain('other: gcs-auth');
	});
	it('errors when the last attempt failed even with no error kind', () => {
		const h = healthy({
			backups: {
				enabled: true,
				updated: null,
				databases: [db({ last_attempt_ok: false })],
				schedules: [],
			},
		});
		expect(chip(h, 'backups').level).toBe('error');
	});
	it('warns when a success is older than twice the interval', () => {
		const old = db({ last_success: '2026-10-06T00:00:00Z' });
		expect(isStale(old, NOW)).toBe(true);
		expect(isStale(db(), NOW)).toBe(false);
		const h = healthy({
			backups: { enabled: true, updated: null, databases: [old], schedules: [] },
		});
		expect(chip(h, 'backups').level).toBe('warn');
	});
	it('does not judge an unknown schedule', () => {
		expect(isStale(db({ schedule: 'fortnightly', last_success: null }), NOW)).toBe(false);
	});
	it('is unknown when backups are not configured', () => {
		expect(chip(healthy({ backups: undefined }), 'backups').level).toBe('unknown');
	});
});

describe('assess: units', () => {
	it('errors on a failed tunnel', () => {
		const h = healthy({
			units: [unit({ active_state: 'failed', sub_state: 'failed', result: 'exit-code' })],
		});
		const c = chip(h, 'units');
		expect(c.level).toBe('error');
		expect(c.note).toContain('devotee-db-tunnel.service');
	});
	it('errors on a failed backup timer', () => {
		const t = unit({
			name: 'ikenga-backup-daily.timer',
			kind: 'timer',
			sub_state: 'dead',
			active_state: 'failed',
			result: null,
		});
		expect(chip(healthy({ units: [t] }), 'units').level).toBe('error');
	});
	it('errors on a failed backup run instance', () => {
		const r = unit({
			name: 'ikenga-backup@daily.service',
			kind: 'backup_run',
			active_state: 'failed',
			result: 'exit-code',
		});
		expect(chip(healthy({ units: [r] }), 'units').level).toBe('error');
	});
	it('warns on an inactive tunnel and is ok when all run', () => {
		expect(
			chip(healthy({ units: [unit({ active_state: 'inactive', sub_state: 'dead' })] }), 'units')
				.level
		).toBe('warn');
		expect(chip(healthy({ units: [unit()] }), 'units').level).toBe('ok');
	});
});

describe('assess: overall', () => {
	it('is the worst chip', () => {
		expect(assess(healthy(), NOW).overall).toBe('ok');
		const warn = healthy({ disk: { total_bytes: 100 * GB, free_bytes: 9 * GB } });
		expect(assess(warn, NOW).overall).toBe('warn');
		const err = healthy({
			disk: { total_bytes: 100 * GB, free_bytes: 9 * GB },
			units: [unit({ active_state: 'failed', result: 'exit-code' })],
		});
		expect(assess(err, NOW).overall).toBe('error');
	});
	it('a snapshot with every section absent has no green claim to make', () => {
		const bare: ServerHealth = {
			schema: 1,
			taken_at_ms: NOW,
			version: '1',
			tier: 't0',
			unavailable: ['everything'],
		};
		for (const c of assess(bare, NOW).chips) expect(c.level).toBe('unknown');
	});
});

describe('formatting', () => {
	it('bytes', () => {
		expect(fmtBytes(0)).toBe('0 B');
		expect(fmtBytes(1536)).toBe('1.5 KB');
		expect(fmtBytes(3.8 * GB)).toBe('3.8 GB');
		expect(fmtBytes(-1)).toBe('—');
	});
	it('uptime', () => {
		expect(fmtUptime(59)).toBe('0m');
		expect(fmtUptime(3 * 3600 + 120)).toBe('3h 2m');
		expect(fmtUptime(86_400 * 2 + 3600 * 5)).toBe('2d 5h');
	});
});
