#!/usr/bin/env bun
/**
 * Launch smoke gate — starts the built app and proves it can actually be driven.
 *
 * Every gate in CI verifies that the code compiles and its units pass. None of
 * them ever *start* the app. That is how v0.8.0 shipped with a dead iyke
 * FE⇄backend channel — typecheck, both cargo checks, 786 Rust tests, 804
 * frontend tests, CI and a 4-leg release build were all green, and no terminal
 * could spawn (ikenga#140). This gate closes that hole: it launches the real
 * binary and asks it to do something that requires the frontend and the backend
 * to be talking.
 *
 * The load-bearing probe is `GET /iyke/dom`. It round-trips backend → FE
 * listener → `invoke('iyke_dom_done')` → backend, so it fails on exactly the
 * class of break that #140 was and passes on v0.7.2.
 *
 * `GET /iyke/state` is checked too, but note *what* is checked: on broken
 * v0.8.0 that endpoint returned HTTP 200 with valid JSON and every shell field
 * `null`. Asserting status and parseability would have passed the broken build,
 * so this asserts `shell.mode` and `shell.route` are actually populated.
 *
 * ## Why it launches twice
 *
 * A fresh data dir is a first run, and `routes/__root.tsx` bounces a first run
 * to `/onboarding`, which deliberately renders *without* the Workspace chrome —
 * and `useIykeBridge` is mounted by Workspace. So on a virgin profile the bridge
 * legitimately does not exist and the probe would fail on a perfectly good
 * build. Phase 1 launches once purely to let the app create and migrate its
 * SQLite database; we then seed `settings_kv['onboarding.state']` as completed
 * and relaunch. Phase 2 is the one that gets probed.
 *
 * Usage:
 *   bun run scripts/launch-smoke-gate.ts --binary=/path/to/ikenga-desktop
 *   bun run scripts/launch-smoke-gate.ts            # probe an already-running app
 *
 * Needs a display. In CI, wrap it: `xvfb-run -a bun run scripts/...`.
 *
 * Exit: 0 pass, 1 fail.
 */

import { Database } from 'bun:sqlite';
import { spawn, type Subprocess } from 'bun';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { homedir, platform, tmpdir } from 'node:os';
import { join } from 'node:path';

interface ControlJson {
	port: number;
	token?: string;
	bearer_token?: string;
	pid?: number;
}

const IDENTIFIER = 'app.ikenga';

function controlJsonPathFor(dataHome: string | null): string {
	if (dataHome) return join(dataHome, IDENTIFIER, 'control.json');
	const home = homedir();
	const sys = platform();
	if (sys === 'darwin')
		return join(home, 'Library', 'Application Support', IDENTIFIER, 'control.json');
	if (sys === 'win32')
		return join(process.env.LOCALAPPDATA || join(home, 'AppData', 'Local'), IDENTIFIER, 'control.json');
	return join(process.env.XDG_DATA_HOME || join(home, '.local', 'share'), IDENTIFIER, 'control.json');
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

function fail(msg: string): never {
	console.error(`[smoke-gate] FAIL: ${msg}`);
	process.exit(1);
}

/** Poll for control.json and return it once it carries a port. */
async function waitForControl(path: string, deadline: number): Promise<ControlJson | null> {
	while (Date.now() < deadline) {
		if (existsSync(path)) {
			try {
				const c = JSON.parse(readFileSync(path, 'utf8')) as ControlJson;
				if (c?.port) return c;
			} catch {
				// half-written file; the app rewrites it atomically, so just retry
			}
		}
		await sleep(300);
	}
	return null;
}

function launch(binary: string, dataHome: string): Subprocess {
	return spawn([binary], {
		env: { ...process.env, XDG_DATA_HOME: dataHome, RUST_LOG: process.env.RUST_LOG ?? 'info' },
		stdout: 'pipe',
		stderr: 'pipe',
	});
}

async function stop(proc: Subprocess): Promise<void> {
	proc.kill();
	await Promise.race([proc.exited, sleep(5000)]);
}

/**
 * Mark onboarding complete so the relaunch renders Workspace (and therefore
 * mounts the iyke bridge) instead of the edge-to-edge onboarding route.
 *
 * `mode: 'edit'` + a non-null `completedAt` is what `__root.tsx` checks; the
 * shape mirrors a real completed wizard so the store's migration path accepts
 * it rather than falling back to defaults.
 */
function seedOnboardingComplete(dataHome: string): void {
	const db = join(dataHome, IDENTIFIER, 'ikenga.db');
	if (!existsSync(db)) fail(`app never created its database at ${db}`);
	const now = Date.now();
	const state = JSON.stringify({
		version: 2,
		startedAt: now,
		completedAt: now,
		mode: 'edit',
		activeIndex: 0,
		steps: {},
	});
	const conn = new Database(db);
	try {
		conn.run(
			'INSERT INTO settings_kv (key, value, updated_at) VALUES (?, ?, ?) ' +
				'ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at',
			['onboarding.state', state, now]
		);
	} finally {
		conn.close();
	}
}

/**
 * The real check. Retries until the deadline because `control.json` is written
 * when the Rust bridge binds, which is before the webview has mounted — a
 * healthy app answers within seconds, a broken one never does.
 */
async function probe(control: ControlJson, deadline: number): Promise<void> {
	const headers: Record<string, string> = {};
	const token = control.token || control.bearer_token;
	if (token) headers.Authorization = `Bearer ${token}`;
	const base = `http://127.0.0.1:${control.port}`;

	let lastErr = 'never attempted';
	while (Date.now() < deadline) {
		try {
			const res = await fetch(`${base}/iyke/dom`, { headers });
			if (res.ok) {
				const body = (await res.json()) as unknown;
				if (body && typeof body === 'object') {
					console.log('[smoke-gate] PASS: /iyke/dom answered — the FE bridge is live');
					break;
				}
				lastErr = '/iyke/dom returned non-object JSON';
			} else {
				// 500/503 here is the #140 signature: the backend emitted the
				// request and the frontend never resolved it.
				lastErr = `/iyke/dom returned HTTP ${res.status}: ${(await res.text()).slice(0, 200)}`;
			}
		} catch (err) {
			lastErr = `/iyke/dom fetch error: ${String(err)}`;
		}
		await sleep(1000);
	}
	if (Date.now() >= deadline) fail(`${lastErr} (this is the ikenga#140 signature)`);

	const res = await fetch(`${base}/iyke/state`, { headers });
	if (!res.ok) fail(`/iyke/state returned HTTP ${res.status}`);
	const state = (await res.json()) as { shell?: { mode?: unknown; route?: unknown } };
	const shell = state?.shell ?? {};
	// Deliberately assert the CONTENT: broken v0.8.0 served 200 + valid JSON
	// with every one of these null, so a status check alone proves nothing.
	if (shell.mode == null || shell.route == null) {
		fail(
			`/iyke/state answered but the shell never published — mode=${JSON.stringify(shell.mode)} route=${JSON.stringify(shell.route)}`
		);
	}
	console.log(`[smoke-gate] PASS: /iyke/state populated — mode=${shell.mode} route=${shell.route}`);
}

async function main(): Promise<void> {
	const arg = (name: string): string | undefined =>
		process.argv.find((a) => a.startsWith(`--${name}=`))?.split('=').slice(1).join('=');

	const timeoutSec = Number.parseInt(arg('timeout-sec') ?? '90', 10);
	const binary = arg('binary');

	// Probe-only mode: talk to whatever is already running. Handy locally.
	if (!binary) {
		const path = controlJsonPathFor(null);
		console.log(`[smoke-gate] probe-only; reading ${path}`);
		const control = await waitForControl(path, Date.now() + timeoutSec * 1000);
		if (!control) fail(`no usable control.json at ${path} within ${timeoutSec}s`);
		await probe(control, Date.now() + timeoutSec * 1000);
		console.log('[smoke-gate] SUCCESS');
		return;
	}

	if (!existsSync(binary)) fail(`binary not found: ${binary}`);
	const dataHome = mkdtempSync(join(tmpdir(), 'ikenga-smoke-'));
	const controlPath = controlJsonPathFor(dataHome);
	let proc: Subprocess | null = null;

	try {
		// ── Phase 1: let it build + migrate its database on a virgin profile.
		console.log(`[smoke-gate] phase 1: first boot (XDG_DATA_HOME=${dataHome})`);
		proc = launch(binary, dataHome);
		const first = await waitForControl(controlPath, Date.now() + timeoutSec * 1000);
		if (!first) {
			const err = await new Response(proc.stderr as ReadableStream).text().catch(() => '');
			fail(`app never wrote control.json on first boot within ${timeoutSec}s.\n${err.slice(-2000)}`);
		}
		console.log('[smoke-gate] phase 1 ok — backend booted');
		await stop(proc);
		proc = null;

		// A first run renders /onboarding, which bypasses Workspace and so never
		// mounts the bridge. Skip past it before probing.
		seedOnboardingComplete(dataHome);
		console.log('[smoke-gate] seeded onboarding as complete');

		// ── Phase 2: the boot that actually gets driven.
		//
		// Drop phase 1's control.json first. It survives the process it
		// described, so leaving it in place means `waitForControl` returns
		// instantly with a dead port and every probe fails with a connection
		// error that looks exactly like a hung frontend.
		rmSync(controlPath, { force: true });
		console.log('[smoke-gate] phase 2: relaunch and probe');
		proc = launch(binary, dataHome);
		const second = await waitForControl(controlPath, Date.now() + timeoutSec * 1000);
		if (!second) fail(`app never wrote control.json on relaunch within ${timeoutSec}s`);
		await probe(second, Date.now() + timeoutSec * 1000);

		console.log('[smoke-gate] SUCCESS: the built app launches and is drivable');
	} finally {
		if (proc) await stop(proc);
		try {
			rmSync(dataHome, { recursive: true, force: true });
		} catch {
			// temp dir cleanup is best-effort; CI runners are ephemeral anyway
		}
	}
}

await main();
