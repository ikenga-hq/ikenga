#!/usr/bin/env bun
// WP-19 — frame live probe (DEC-27, second harness).
//
// The browser-mode Playwright harness (`bun run test:e2e`) proves everything
// that renders against a mocked host. This script proves what a mock cannot:
// it drives the REAL running shell through the iyke localhost bridge (plain
// HTTP — no `iyke` binary needed) and checks the host round-trips the Phase 1
// frame depends on.
//
//   bridge            GET  /iyke/state answers with the typed state shape
//   frame-state       the frontend has pushed mode + a pane tree to the bridge
//   project-switch    POST /iyke/project/set-active → Rust persists it
//                     (/iyke/project/active) AND the frontend re-renders
//                     (/iyke/dom shows the rail's "Project: <name>" button);
//                     if /iyke/state carries shell.active_project (WP-21)
//                     that is checked too. The original project is restored.
//   pty-dispatch      spawn a terminal, write a command into its PTY, wait for
//                     the command's OUTPUT (not the echoed input), kill it.
//   chi-list          GET /iyke/chi/list — the Chi runtime answers.
//   chi-run           POST /iyke/chi/run + poll /iyke/chi/status. Opt-in
//                     (`--with-chi`): it starts a real agent run.
//   v15-migration     real v15 → v16 migration on a copied profile. Needs a
//                     shell launched against that copy (`--profile=<dir>`,
//                     lands with WP-02); BLOCKED until then.
//
// Modelled on scripts/engine-wire-smoke.ts: each check reports
//   PASS    — verified against the live shell
//   BLOCKED — a prerequisite is missing (no shell running, opt-in flag not
//             given, producer WP not merged); not a regression
//   FAIL    — the shell answered and the answer was wrong
// Exit: 0 unless at least one check FAILed.
//
// Usage:
//   bun run probe:frame [--read-only] [--with-chi] [--control=<path>]
//                       [--only=bridge,pty-dispatch,…] [--timeout-ms=<n>]
//   --read-only   skip the checks that change shell state (project-switch,
//                 pty-dispatch) — they report BLOCKED instead.
//   --control     control.json to read instead of the platform default. Env
//                 IKENGA_IYKE_URL + IKENGA_IYKE_TOKEN override discovery.
//
// Never prints the bridge token.

import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';

type Outcome = 'PASS' | 'BLOCKED' | 'FAIL';
interface Result {
	check: string;
	outcome: Outcome;
	detail: string;
}

const CHECKS = [
	'bridge',
	'frame-state',
	'project-switch',
	'pty-dispatch',
	'chi-list',
	'chi-run',
	'v15-migration',
] as const;
type CheckId = (typeof CHECKS)[number];

// ── CLI ──────────────────────────────────────────────────────────────────────

function flag(name: string): boolean {
	return process.argv.includes(`--${name}`);
}
function arg(name: string): string | undefined {
	const hit = process.argv.find((a) => a.startsWith(`--${name}=`));
	return hit ? hit.slice(name.length + 3) : undefined;
}

const READ_ONLY = flag('read-only');
const WITH_CHI = flag('with-chi');
const TIMEOUT_MS = Number(arg('timeout-ms') ?? 10_000);
const ONLY = new Set((arg('only') ?? CHECKS.join(',')).split(',').map((s) => s.trim()));

// ── Bridge discovery ─────────────────────────────────────────────────────────

/** Same path the Rust side writes (`app_local_data_dir()/control.json`) and
 *  `iyke-cli/src/control.rs` reads (`dirs::data_local_dir()/app.ikenga`). */
function defaultControlPath(): string {
	const id = 'app.ikenga';
	if (process.platform === 'win32') {
		const base = process.env.LOCALAPPDATA ?? join(homedir(), 'AppData', 'Local');
		return join(base, id, 'control.json');
	}
	if (process.platform === 'darwin') {
		return join(homedir(), 'Library', 'Application Support', id, 'control.json');
	}
	const base = process.env.XDG_DATA_HOME || join(homedir(), '.local', 'share');
	return join(base, id, 'control.json');
}

interface Bridge {
	url: string;
	token: string;
	source: string;
	pid?: number;
}

type Discovery = { ok: true; bridge: Bridge } | { ok: false; reason: string };

function discover(): Discovery {
	const envUrl = process.env.IKENGA_IYKE_URL;
	const envToken = process.env.IKENGA_IYKE_TOKEN;
	if (envUrl && envToken) {
		return { ok: true, bridge: { url: envUrl.replace(/\/$/, ''), token: envToken, source: 'env' } };
	}
	const path = arg('control') ?? defaultControlPath();
	if (!existsSync(path)) {
		return {
			ok: false,
			reason: `no running shell — ${path} not found (start one: bun run tauri dev)`,
		};
	}
	let parsed: { port?: number; token?: string; pid?: number };
	try {
		parsed = JSON.parse(readFileSync(path, 'utf8'));
	} catch (e) {
		return { ok: false, reason: `unreadable ${path}: ${String(e)}` };
	}
	if (typeof parsed.port !== 'number' || typeof parsed.token !== 'string') {
		return { ok: false, reason: `${path} has no port/token` };
	}
	return {
		ok: true,
		bridge: {
			url: `http://127.0.0.1:${parsed.port}`,
			token: parsed.token,
			source: path,
			pid: parsed.pid,
		},
	};
}

function pidAlive(pid: number | undefined): boolean | null {
	if (!pid) return null;
	try {
		process.kill(pid, 0);
		return true;
	} catch (e) {
		// EPERM: exists but not ours to signal — still alive.
		return (e as NodeJS.ErrnoException).code === 'EPERM';
	}
}

// ── HTTP ─────────────────────────────────────────────────────────────────────

class BridgeUnreachable extends Error {}

interface Reply<T> {
	status: number;
	body: T;
	text: string;
}

async function call<T = any>(
	b: Bridge,
	method: 'GET' | 'POST',
	path: string,
	body?: unknown,
	timeoutMs = TIMEOUT_MS
): Promise<Reply<T>> {
	let res: Response;
	try {
		res = await fetch(`${b.url}${path}`, {
			method,
			headers: {
				Authorization: `Bearer ${b.token}`,
				...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
			},
			body: body === undefined ? undefined : JSON.stringify(body),
			signal: AbortSignal.timeout(timeoutMs),
		});
	} catch (e) {
		throw new BridgeUnreachable(`${method} ${path}: ${(e as Error).message}`);
	}
	const text = await res.text();
	let parsed: unknown = null;
	try {
		parsed = text ? JSON.parse(text) : null;
	} catch {
		parsed = null;
	}
	return { status: res.status, body: parsed as T, text };
}

function brief(r: Reply<unknown>): string {
	return `HTTP ${r.status}${r.text ? ` ${r.text.slice(0, 160)}` : ''}`;
}

async function until<T>(
	fn: () => Promise<T | undefined>,
	ms: number,
	every = 250
): Promise<T | undefined> {
	const deadline = Date.now() + ms;
	for (;;) {
		const v = await fn();
		if (v !== undefined) return v;
		if (Date.now() >= deadline) return undefined;
		await new Promise((r) => setTimeout(r, every));
	}
}

// ── Checks ───────────────────────────────────────────────────────────────────

interface StateBody {
	schema_version?: number;
	app?: { pid?: number };
	shell?: {
		mode?: string | null;
		route?: string | null;
		panes?: { leaves?: unknown[] } | null;
		sidebar_collapsed?: boolean | null;
		active_project?: unknown;
	};
}

async function checkBridge(b: Bridge): Promise<Result> {
	const r = await call<StateBody>(b, 'GET', '/iyke/state');
	if (r.status === 401) {
		return {
			check: 'bridge',
			outcome: 'BLOCKED',
			detail: 'bridge rejected the control.json token (stale file from an earlier launch?)',
		};
	}
	if (r.status !== 200 || !r.body) return { check: 'bridge', outcome: 'FAIL', detail: brief(r) };
	if (r.body.schema_version !== 1 || typeof r.body.shell !== 'object') {
		return {
			check: 'bridge',
			outcome: 'FAIL',
			detail: `unexpected state shape: ${r.text.slice(0, 160)}`,
		};
	}
	return {
		check: 'bridge',
		outcome: 'PASS',
		detail: `state schema_version=1 from pid ${r.body.app?.pid ?? '?'}`,
	};
}

async function checkFrameState(b: Bridge): Promise<Result> {
	const r = await call<StateBody>(b, 'GET', '/iyke/state');
	const shell = r.body?.shell;
	if (!shell) return { check: 'frame-state', outcome: 'FAIL', detail: brief(r) };
	if (shell.mode == null && shell.panes == null) {
		return {
			check: 'frame-state',
			outcome: 'BLOCKED',
			detail: 'frontend has not pushed shell state yet (still booting?)',
		};
	}
	const leaves = shell.panes?.leaves;
	if (!Array.isArray(leaves) || leaves.length === 0) {
		return {
			check: 'frame-state',
			outcome: 'FAIL',
			detail: `mode=${shell.mode} but the pushed pane tree has no leaves`,
		};
	}
	return {
		check: 'frame-state',
		outcome: 'PASS',
		detail: `mode=${shell.mode} route=${shell.route} leaves=${leaves.length} sidebar_collapsed=${shell.sidebar_collapsed}`,
	};
}

interface ProjectRow {
	id: string;
	display_name: string;
	archived_at: number | null;
}

async function activeProjectId(b: Bridge): Promise<string | undefined> {
	const r = await call<{ project?: ProjectRow }>(b, 'GET', '/iyke/project/active');
	return r.body?.project?.id;
}

/** The rail's project indicator is a button whose accessible name starts
 *  with `Project: <display_name>` (activity-bar.tsx ProjectIndicator). */
async function domShowsProject(b: Bridge, name: string): Promise<boolean> {
	const r = await call<{ text?: string }>(
		b,
		'GET',
		`/iyke/dom?query=${encodeURIComponent(`Project: ${name}`)}`
	);
	return (
		r.status === 200 && typeof r.body?.text === 'string' && r.body.text.includes(`Project: ${name}`)
	);
}

async function checkProjectSwitch(b: Bridge): Promise<Result> {
	const check = 'project-switch';
	if (READ_ONLY) return { check, outcome: 'BLOCKED', detail: '--read-only: switch not attempted' };

	const list = await call<{ projects?: ProjectRow[] }>(b, 'GET', '/iyke/project/list');
	if (list.status !== 200 || !Array.isArray(list.body?.projects)) {
		return { check, outcome: 'FAIL', detail: `project/list: ${brief(list)}` };
	}
	const live = list.body.projects.filter((p) => p.archived_at == null);
	const original = await activeProjectId(b);
	if (!original) return { check, outcome: 'FAIL', detail: 'project/active returned no project' };
	const target = live.find((p) => p.id !== original);
	if (!target) {
		return {
			check,
			outcome: 'BLOCKED',
			detail: `needs ≥2 live projects to switch between (found ${live.length}); create one in Settings → Projects`,
		};
	}

	const notes: string[] = [];
	try {
		const set = await call(b, 'POST', '/iyke/project/set-active', { id: target.id });
		if (set.status !== 200) return { check, outcome: 'FAIL', detail: `set-active: ${brief(set)}` };

		const persisted = await until(
			async () => ((await activeProjectId(b)) === target.id ? true : undefined),
			3_000
		);
		if (!persisted) {
			return { check, outcome: 'FAIL', detail: `Rust did not persist ${target.id} as active` };
		}

		// Rust → `projects:active-changed` → frontend store → rail re-render.
		const rendered = await until(
			async () => ((await domShowsProject(b, target.display_name)) ? true : undefined),
			5_000,
			400
		);
		if (!rendered) {
			return {
				check,
				outcome: 'FAIL',
				detail: `frontend never rendered "Project: ${target.display_name}" after the switch`,
			};
		}

		// WP-21 adds shell.active_project to /iyke/state; check it once present.
		const st = await call<StateBody>(b, 'GET', '/iyke/state');
		const ap = st.body?.shell?.active_project;
		if (ap === undefined) {
			notes.push('state has no shell.active_project yet (WP-21)');
		} else {
			const apId = typeof ap === 'string' ? ap : (ap as { id?: string } | null)?.id;
			if (apId !== target.id) {
				return {
					check,
					outcome: 'FAIL',
					detail: `state.shell.active_project=${JSON.stringify(ap)} after switching to ${target.id}`,
				};
			}
			notes.push('state.shell.active_project follows');
		}
	} finally {
		const back = await call(b, 'POST', '/iyke/project/set-active', { id: original }).catch(
			() => null
		);
		if (back?.status !== 200) {
			console.error(
				`! project-switch: could not restore active project "${original}" — reset it by hand`
			);
		}
	}
	return {
		check,
		outcome: 'PASS',
		detail: `${original} → ${target.id}: persisted + rail re-rendered; restored${notes.length ? ` (${notes.join('; ')})` : ''}`,
	};
}

async function checkPtyDispatch(b: Bridge): Promise<Result> {
	const check = 'pty-dispatch';
	if (READ_ONLY) return { check, outcome: 'BLOCKED', detail: '--read-only: no terminal spawned' };

	// Match the command's OUTPUT, never its echoed input: the typed line holds
	// an expression, only the shell's evaluation of it contains "_42".
	const win = process.platform === 'win32';
	const argv = win ? ['powershell.exe', '-NoLogo', '-NoProfile'] : ['/bin/sh'];
	const line = win ? `Write-Output ("FRAME_PROBE_" + (6*7))` : 'echo FRAME_PROBE_$((6*7))';
	const label = `frame-probe-${Date.now()}`;

	const spawn = await call<{ pty_id?: string; terminal_id?: string }>(
		b,
		'POST',
		'/iyke/terminal/spawn',
		{ argv, title: 'frame probe', label },
		20_000
	);
	if (spawn.status !== 200 || !spawn.body?.pty_id) {
		return { check, outcome: 'FAIL', detail: `terminal/spawn: ${brief(spawn)}` };
	}
	const pty = spawn.body.pty_id;
	try {
		const send = await call(b, 'POST', '/iyke/terminal/send', {
			terminal: pty,
			data: line,
			keys: ['Enter'],
		});
		if (send.status !== 200)
			return { check, outcome: 'FAIL', detail: `terminal/send: ${brief(send)}` };

		const wait = await call<{ matched?: boolean; timed_out?: boolean; exited?: boolean }>(
			b,
			'POST',
			'/iyke/terminal/wait',
			{ terminal: pty, match: 'FRAME_PROBE_42', timeout_ms: 15_000 },
			20_000
		);
		if (wait.status !== 200)
			return { check, outcome: 'FAIL', detail: `terminal/wait: ${brief(wait)}` };
		if (!wait.body?.matched) {
			return {
				check,
				outcome: 'FAIL',
				detail: `PTY never printed FRAME_PROBE_42 (timed_out=${wait.body?.timed_out}, exited=${wait.body?.exited})`,
			};
		}
	} finally {
		await call(b, 'POST', '/iyke/terminal/kill', { terminal: pty, close_tab: true }).catch(
			() => null
		);
	}
	return {
		check,
		outcome: 'PASS',
		detail: `spawned ${argv[0]} (${label}), wrote a command, read its output, killed it`,
	};
}

async function checkChiList(b: Bridge): Promise<Result> {
	const r = await call<unknown[]>(b, 'GET', '/iyke/chi/list?limit=5');
	if (r.status !== 200 || !Array.isArray(r.body)) {
		return { check: 'chi-list', outcome: 'FAIL', detail: brief(r) };
	}
	return {
		check: 'chi-list',
		outcome: 'PASS',
		detail: `chi runtime answered (${r.body.length} recent runs)`,
	};
}

async function checkChiRun(b: Bridge): Promise<Result> {
	const check = 'chi-run';
	if (!WITH_CHI) {
		return {
			check,
			outcome: 'BLOCKED',
			detail: 'opt-in: pass --with-chi (starts a real agent run)',
		};
	}
	const engine = arg('chi-engine') ?? 'claude-code';
	const run = await call<{ run_id?: string; status?: string; error?: string }>(
		b,
		'POST',
		'/iyke/chi/run',
		// ChiRunOpts is camelCase on the wire (commands/chi.rs).
		{ engineId: engine, prompt: 'Reply with exactly: FRAME-PROBE-OK', persistent: false },
		30_000
	);
	if (run.status !== 200 || !run.body?.run_id) {
		return { check, outcome: 'FAIL', detail: `chi/run: ${brief(run)}` };
	}
	const id = run.body.run_id;
	const final = await until(
		async () => {
			const s = await call<{ status?: string; error?: string }>(
				b,
				'GET',
				`/iyke/chi/status?runId=${encodeURIComponent(id)}`
			);
			const status = s.body?.status;
			return status && status !== 'running' ? s.body : undefined;
		},
		120_000,
		2_000
	);
	if (!final) return { check, outcome: 'FAIL', detail: `run ${id} still not finished after 120s` };
	// Terminal states in chi_cache: done | failed | cancelled.
	if (final.status !== 'done') {
		return {
			check,
			outcome: 'FAIL',
			detail: `run ${id} ended ${final.status}: ${final.error ?? ''}`,
		};
	}
	return { check, outcome: 'PASS', detail: `run ${id} (${engine}) ${final.status}` };
}

function checkMigration(): Result {
	const profile = arg('profile');
	return {
		check: 'v15-migration',
		outcome: 'BLOCKED',
		detail: profile
			? `--profile=${profile} given, but the v15 → v16 migration check lands with WP-02`
			: 'needs a shell launched on a copied v15 profile (--profile=<dir>); lands with WP-02',
	};
}

// ── Main ─────────────────────────────────────────────────────────────────────

const RUNNERS: Record<CheckId, (b: Bridge) => Promise<Result>> = {
	bridge: checkBridge,
	'frame-state': checkFrameState,
	'project-switch': checkProjectSwitch,
	'pty-dispatch': checkPtyDispatch,
	'chi-list': checkChiList,
	'chi-run': checkChiRun,
	'v15-migration': async () => checkMigration(),
};

function report(results: Result[], header: string): never {
	console.log(`\nWP-19 frame live probe\n──────────────────────\n${header}\n`);
	let fail = 0;
	let pass = 0;
	let blocked = 0;
	for (const r of results) {
		const icon = r.outcome === 'PASS' ? '✓' : r.outcome === 'BLOCKED' ? '⊘' : '✗';
		console.log(`${icon} ${r.check.padEnd(15)} ${r.outcome.padEnd(8)} ${r.detail}`);
		if (r.outcome === 'FAIL') fail++;
		else if (r.outcome === 'PASS') pass++;
		else blocked++;
	}
	const verdict = fail > 0 ? 'FAIL' : pass > 0 ? 'PASS' : 'BLOCKED';
	console.log(`\n${verdict} — ${pass} pass, ${blocked} blocked, ${fail} fail\n`);
	process.exit(fail > 0 ? 1 : 0);
}

async function main(): Promise<void> {
	const selected = CHECKS.filter((c) => ONLY.has(c));
	const found = discover();
	if (!found.ok) {
		report(
			selected.map((check) => ({ check, outcome: 'BLOCKED' as const, detail: found.reason })),
			'bridge: not found'
		);
	}
	const bridge = found.bridge;
	const header = `bridge: ${bridge.url} (from ${bridge.source})`;

	// A control.json outlives a crashed shell. Tell "nothing running" apart
	// from "running but broken" before any check can call it a FAIL.
	const alive = pidAlive(bridge.pid);
	try {
		await call(bridge, 'GET', '/iyke/state', undefined, 3_000);
	} catch (e) {
		const why =
			alive === false
				? `no running shell — control.json is stale (pid ${bridge.pid} is gone)`
				: `shell not reachable at ${bridge.url} (${(e as Error).message})`;
		report(
			selected.map((check) => ({ check, outcome: 'BLOCKED' as const, detail: why })),
			header
		);
	}

	const results: Result[] = [];
	for (const check of selected) {
		try {
			results.push(await RUNNERS[check](bridge));
		} catch (e) {
			// The shell went away mid-run — that is a missing prerequisite, not a
			// wrong answer. Anything else thrown is a probe bug worth failing on.
			results.push({
				check,
				outcome: e instanceof BridgeUnreachable ? 'BLOCKED' : 'FAIL',
				detail: (e as Error).message,
			});
		}
		if (check === 'bridge' && results[results.length - 1]?.outcome !== 'PASS') {
			// Nothing downstream can mean anything without a working bridge.
			for (const rest of selected.filter((c) => c !== 'bridge')) {
				results.push({ check: rest, outcome: 'BLOCKED', detail: 'bridge check did not pass' });
			}
			break;
		}
	}
	report(results, header);
}

void main();
