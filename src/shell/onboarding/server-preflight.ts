// Server preflight — the welcome step's report in a browser session.
//
// The desktop preflight is `detect_system`, which the daemon deliberately
// does not serve (`desktop_only.toml` [detect_system], WP-19): half of that
// report is the desktop vault and keychain, which a server does not have.
// So a browser tab builds its report from what the daemon actually answers:
//
//   - `GET /api/health`       — name, version, status, uptime, executor tier,
//                               `principal_isolation`, and the T1 probe stamp.
//   - the session's identity  — `/auth/me` at T1, the paired-device marker,
//                               or the T0 access token.
//   - `list_claude_projects`  — served; the same `claude_projects` row the
//                               desktop shows, read from the server's home.
//
// Rows the desktop shows that a browser cannot check (disk space, the
// app-data folder, the secrets store) are left out rather than guessed.

import {
	type ServerHealth,
	type SystemCheck,
	type SystemReport,
	fetchServerHealth,
	listClaudeProjects,
} from '@/lib/tauri-cmd';
import { isDeviceSession } from '@/lib/transport/device-session';
import {
	type AuthMe,
	currentPrincipal,
	fetchAuthMe,
	isT1Session,
} from '@/lib/transport/t1-session';

/** What the welcome step renders: the desktop `SystemReport` satisfies it,
 *  and so does the server report built here. */
export type PreflightReport = Pick<SystemReport, 'checks'>;

/** How this browser tab is signed in to the server. */
export type ServerAccess =
	| { kind: 't1'; principal: AuthMe | null }
	| { kind: 'device' }
	| { kind: 'token' };

/** Claude Code project count on the server, or why it could not be read. */
export type ClaudeProjectsResult = { count: number } | { error: string };

export interface ServerPreflightInput {
	health: ServerHealth;
	access: ServerAccess;
	claudeProjects: ClaudeProjectsResult;
}

/** Gather the inputs over the daemon's served surface and build the report.
 *  Only a failed health read fails the whole report; the project listing
 *  degrades to a warning row (a paired device may lack the `files` cap). */
export async function detectServerPreflight(): Promise<PreflightReport> {
	const [health, access, claudeProjects] = await Promise.all([
		fetchServerHealth(),
		readAccess(),
		listClaudeProjects().then(
			(rows): ClaudeProjectsResult => ({ count: rows.length }),
			(err: unknown): ClaudeProjectsResult => ({
				error: err instanceof Error ? err.message : String(err),
			})
		),
	]);
	return { checks: buildServerChecks({ health, access, claudeProjects }) };
}

async function readAccess(): Promise<ServerAccess> {
	if (isT1Session()) {
		return { kind: 't1', principal: currentPrincipal() ?? (await fetchAuthMe()) };
	}
	if (isDeviceSession()) return { kind: 'device' };
	return { kind: 'token' };
}

/** Pure: the rows for a server report. No row is `fail` — nothing on this
 *  list is something the person at the browser can fix themselves. */
export function buildServerChecks(input: ServerPreflightInput): SystemCheck[] {
	return [
		serverRow(input.health),
		accessRow(input.access),
		sessionsRow(input.health),
		claudeProjectsRow(input.claudeProjects),
	];
}

function serverRow(health: ServerHealth): SystemCheck {
	const name = health.name || 'ikenga-server';
	const version = health.version ? ` ${health.version}` : '';
	if (health.ok && health.status === 'ready') {
		return {
			id: 'server',
			level: 'pass',
			message: `${name}${version} is ready · up ${formatUptime(health.uptime_secs)}`,
			fix_hint: null,
		};
	}
	return {
		id: 'server',
		level: 'warn',
		message: `${name}${version} reports status "${health.status || 'unknown'}"`,
		fix_hint: 'Wait a moment and re-check. If it stays like this, ask the server’s operator.',
	};
}

function accessRow(access: ServerAccess): SystemCheck {
	if (access.kind === 't1') {
		if (access.principal) {
			const admin = access.principal.is_admin ? ' (admin)' : '';
			return {
				id: 'access',
				level: 'pass',
				message: `Signed in as ${access.principal.username}${admin}`,
				fix_hint: null,
			};
		}
		return {
			id: 'access',
			level: 'warn',
			message: 'Could not confirm who is signed in',
			fix_hint: 'Reload the page and sign in again.',
		};
	}
	if (access.kind === 'device') {
		return {
			id: 'access',
			level: 'pass',
			message: 'This browser is a paired device',
			fix_hint: null,
		};
	}
	return {
		id: 'access',
		level: 'pass',
		message: 'Connected with the server’s access token',
		fix_hint: null,
	};
}

function sessionsRow(health: ServerHealth): SystemCheck {
	const exec = health.executor;
	if (!exec) {
		return {
			id: 'sessions',
			level: 'warn',
			message: 'This server does not say how it runs terminals and agents',
			fix_hint: 'It may be an older version. Ask the server’s operator to update it.',
		};
	}
	// T1 reports isolation only from a passing probe and drops it once the
	// executor degrades — and a degraded executor refuses every spawn
	// (executor/t1.rs, G-PRINCIPAL §8). So at T1 "off" means "refused".
	if (exec.tier === 't1' && !exec.principal_isolation) {
		return {
			id: 'sessions',
			level: 'warn',
			message: 'Per-user isolation is off, so this server is refusing new terminals and agents',
			fix_hint: 'Ask the server’s operator to check its log and restart it.',
		};
	}
	const missing = [!exec.pty && 'terminals', !exec.piped && 'agent CLIs'].filter(Boolean);
	if (missing.length > 0) {
		return {
			id: 'sessions',
			level: 'warn',
			message: `This server cannot start ${missing.join(' or ')}`,
			fix_hint: 'Ask the server’s operator to check how it was set up.',
		};
	}
	if (exec.tier === 't0') {
		return {
			id: 'sessions',
			level: 'pass',
			message: 'Single-user server: terminals and agents run as the server’s own account',
			fix_hint: null,
		};
	}
	if (exec.tier === 't1') {
		const checked = health.probe?.ok ? ` · isolation checked ${formatStamp(health.probe.at)}` : '';
		return {
			id: 'sessions',
			level: 'pass',
			message: `Multi-user server: your terminals and agents run as your own account${checked}`,
			fix_hint: null,
		};
	}
	return {
		id: 'sessions',
		level: 'pass',
		message: `Executor tier ${exec.tier}: per-user isolation ${exec.principal_isolation ? 'on' : 'off'}`,
		fix_hint: null,
	};
}

function claudeProjectsRow(result: ClaudeProjectsResult): SystemCheck {
	if ('error' in result) {
		return {
			id: 'claude_projects',
			level: 'warn',
			message: `Could not list Claude Code projects on the server: ${result.error}`,
			fix_hint: null,
		};
	}
	if (result.count === 0) {
		return {
			id: 'claude_projects',
			level: 'warn',
			message: 'No Claude Code projects on the server yet',
			fix_hint:
				'That’s fine — one appears when you run a Claude Code session on the server for the first time.',
		};
	}
	return {
		id: 'claude_projects',
		level: 'pass',
		message: `${result.count} Claude Code project${result.count === 1 ? '' : 's'} on the server`,
		fix_hint: null,
	};
}

/** `3d 4h`, `5h 12m`, `7m`, or `under a minute`. */
export function formatUptime(secs: number): string {
	const s = Number.isFinite(secs) && secs > 0 ? Math.floor(secs) : 0;
	const d = Math.floor(s / 86_400);
	const h = Math.floor((s % 86_400) / 3_600);
	const m = Math.floor((s % 3_600) / 60);
	if (d > 0) return `${d}d ${h}h`;
	if (h > 0) return `${h}h ${m}m`;
	if (m > 0) return `${m}m`;
	return 'under a minute';
}

/** Unix seconds → `2026-10-06 03:20 UTC` (fixed zone, so it reads the same
 *  for everyone looking at one server). */
function formatStamp(at: number): string {
	const d = new Date(at * 1000);
	if (Number.isNaN(d.getTime())) return 'at boot';
	return `${d.toISOString().slice(0, 16).replace('T', ' ')} UTC`;
}
