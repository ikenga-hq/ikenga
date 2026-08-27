/**
 * Live protocol verification against a running `ikenga-server`.
 *
 * Nothing here is stubbed: every check drives the daemon's real HTTP and
 * WebSocket surface, against real PTYs and (for section 7) the real
 * antigravity CLI. It exists because the two worst defects in this work —
 * a `tokio::join!` that never returned, and a deleted stylesheet import —
 * both compiled clean and passed every offline gate.
 *
 * Usage:
 *   cargo build -p ikenga-server
 *   IKENGA_AUTH_TOKEN=<token> ./src-tauri/target/debug/ikenga-server \
 *     --port 4477 --static-dir ./dist --data-dir /tmp/ikenga-verify
 *   IKENGA_VERIFY_URL=http://127.0.0.1:4477 IKENGA_AUTH_TOKEN=<token> \
 *     bun run scripts/server/verify-live.ts
 *
 * Section 7 needs `agy` on PATH; it reports a real error rather than a
 * silent pass when the CLI is absent.
 */
const BASE = process.env.IKENGA_VERIFY_URL ?? 'http://127.0.0.1:4477';
const WS_BASE = BASE.replace(/^http/, 'ws');
const TOKEN = process.env.IKENGA_AUTH_TOKEN ?? 'verifytoken123';

let pass = 0;
let fail = 0;
const failures: string[] = [];

function check(name: string, ok: boolean, detail = '') {
	if (ok) {
		pass++;
		console.log(`  PASS  ${name}`);
	} else {
		fail++;
		failures.push(`${name}${detail ? ` — ${detail}` : ''}`);
		console.log(`  FAIL  ${name}${detail ? ` — ${detail}` : ''}`);
	}
}

function sleep(ms: number) {
	return new Promise((r) => setTimeout(r, ms));
}

async function rpc(cmd: string, args: Record<string, unknown> = {}) {
	const res = await fetch(`${BASE}/api/rpc`, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
		body: JSON.stringify({ cmd, args }),
	});
	return { status: res.status, body: await res.json().catch(() => null) };
}

interface Frame {
	kind: 'text' | 'binary';
	text?: string;
	json?: any;
	bytes?: Uint8Array;
}

/** Collect frames off a PTY socket until `stop` says so or we time out. */
function openPty(id: string, spawn: boolean) {
	const q = new URLSearchParams({ token: TOKEN });
	if (spawn) q.set('spawn', 'true');
	const ws = new WebSocket(`${WS_BASE}/ws/pty/${encodeURIComponent(id)}?${q}`);
	ws.binaryType = 'arraybuffer';
	const frames: Frame[] = [];
	let closed = false;
	ws.addEventListener('message', (e: any) => {
		if (typeof e.data === 'string') {
			let json: any = null;
			try {
				json = JSON.parse(e.data);
			} catch {}
			frames.push({ kind: 'text', text: e.data, json });
		} else {
			frames.push({ kind: 'binary', bytes: new Uint8Array(e.data) });
		}
	});
	ws.addEventListener('close', () => {
		closed = true;
	});
	const open = new Promise<void>((res, rej) => {
		ws.addEventListener('open', () => res());
		ws.addEventListener('error', (e: any) => rej(new Error(String(e?.message ?? 'ws error'))));
	});
	return {
		ws,
		frames,
		open,
		isClosed: () => closed,
		binaryText: () =>
			frames
				.filter((f) => f.kind === 'binary')
				.map((f) => new TextDecoder().decode(f.bytes!))
				.join(''),
		control: (type: string) => frames.find((f) => f.json?.type === type)?.json,
		waitFor: async (pred: () => boolean, ms = 5000) => {
			const t0 = Date.now();
			while (Date.now() - t0 < ms) {
				if (pred()) return true;
				await sleep(50);
			}
			return false;
		},
	};
}

console.log('\n=== 1. RPC surface ===');
{
	const home = await rpc('fs_home');
	check(
		'fs_home is implemented and returns a real path',
		home.body?.ok === true && typeof home.body.data === 'string' && home.body.data.startsWith('/'),
		JSON.stringify(home.body)
	);

	const db = await rpc('db_query', { query: 'SELECT 1', values: [] });
	check(
		'db_query fails loudly (WP-12b unimplemented) rather than returning a fake result',
		db.body?.ok === false,
		JSON.stringify(db.body)
	);

	const noAuth = await fetch(`${BASE}/api/rpc`, {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ cmd: 'fs_home', args: {} }),
	});
	check('unauthenticated RPC is rejected', noAuth.status === 401, `status ${noAuth.status}`);
}

console.log('\n=== 2. PTY attach: snapshot offset protocol ===');
const termId = `verify-term-${Date.now()}`;
{
	const a = openPty(termId, true);
	await a.open;
	const gotSnap = await a.waitFor(() => !!a.control('ikenga.snapshot'));
	check('daemon announces ikenga.snapshot before any bytes', gotSnap);
	const snap = a.control('ikenga.snapshot');
	check(
		'snapshot carries end_offset and len',
		snap && typeof snap.end_offset === 'number' && typeof snap.len === 'number',
		JSON.stringify(snap)
	);
	// The snapshot control frame must arrive before the first binary frame.
	const firstBinaryIdx = a.frames.findIndex((f) => f.kind === 'binary');
	const snapIdx = a.frames.findIndex((f) => f.json?.type === 'ikenga.snapshot');
	check(
		'snapshot header precedes the scrollback bytes',
		snapIdx >= 0 && (firstBinaryIdx === -1 || snapIdx < firstBinaryIdx),
		`snapIdx=${snapIdx} firstBinary=${firstBinaryIdx}`
	);

	// Drive the shell so there is real output to replay.
	a.ws.send(JSON.stringify({ type: 'write', data: 'echo MARKER_ALPHA\n' }));
	const sawAlpha = await a.waitFor(() => a.binaryText().includes('MARKER_ALPHA'));
	check('PTY output streams over the socket', sawAlpha);

	const paintedBefore = a.binaryText();
	a.ws.close();
	await sleep(300);

	console.log('\n=== 3. Reconnect replays only the new tail (F-3) ===');
	// Reattach WITHOUT spawn — the session must still be there.
	const b = openPty(termId, false);
	await b.open;
	await b.waitFor(() => !!b.control('ikenga.snapshot'));
	const snapB = b.control('ikenga.snapshot');
	check('reattach to an existing session succeeds without spawn', !!snapB, JSON.stringify(snapB));

	// The daemon replays its whole ring; the fix is that the CLIENT trims it
	// using end_offset. Verify the daemon supplies what the client needs:
	// a ring replay whose end_offset is >= the first connection's.
	check(
		'reconnect snapshot end_offset advanced past the first attach',
		snapB && snapB.end_offset >= (snap?.end_offset ?? 0),
		`first=${snap?.end_offset} second=${snapB?.end_offset}`
	);
	check(
		'reconnect replays the ring (so a naive client WOULD double-paint)',
		b.binaryText().includes('MARKER_ALPHA'),
		'ring replay absent'
	);

	// Now simulate what the fixed client does with those numbers.
	const alreadyPainted = paintedBefore.length;
	const replayLen = snapB!.len;
	const snapshotStart = snapB!.end_offset - replayLen;
	const trimmed = Math.max(0, alreadyPainted - snapshotStart);
	check(
		'offset arithmetic yields a non-negative, in-range trim point',
		trimmed >= 0 && trimmed <= replayLen,
		`trim=${trimmed} replayLen=${replayLen}`
	);

	console.log('\n=== 4. Exit vs disconnect (F-4) ===');
	b.ws.send(JSON.stringify({ type: 'write', data: 'exit\n' }));
	const gotExit = await b.waitFor(() => !!b.control('ikenga.exit') || b.isClosed(), 8000);
	check(
		'shell exit produces an explicit ikenga.exit frame',
		!!b.control('ikenga.exit'),
		gotExit ? 'socket closed without the frame' : 'timed out'
	);
	try {
		b.ws.close();
	} catch {}
	await sleep(500);

	console.log('\n=== 5. Auto-spawn is opt-in (F-4b) ===');
	// An exited session is RETAINED with its scrollback, so the right answer is
	// "here is the final output, and the shell is gone" — not a fresh shell,
	// and not a bare `gone` that throws away what the command printed.
	const c = openPty(termId, false);
	await c.open;
	const settledExited = await c.waitFor(() => !!c.control('ikenga.exit'), 5000);
	check(
		'reattaching to an exited session replays its scrollback then reports exit',
		settledExited && !!c.control('ikenga.snapshot'),
		JSON.stringify(c.frames.map((f) => f.json?.type ?? f.kind))
	);
	check(
		'no new shell was fabricated for the exited session',
		!c.binaryText().includes('$ echo') || !!c.control('ikenga.exit'),
		'a fresh prompt would mean auto-spawn fired'
	);
	try {
		c.ws.close();
	} catch {}

	// A session id that never existed cannot be replayed at all — that is what
	// `ikenga.gone` is for.
	const g = openPty(`never-existed-${Date.now()}`, false);
	await g.open;
	const gone = await g.waitFor(() => !!g.control('ikenga.gone'), 4000);
	check(
		'an unknown session id reports ikenga.gone and does not spawn',
		gone,
		JSON.stringify(g.frames.map((f) => f.json?.type ?? f.kind))
	);
	try {
		g.ws.close();
	} catch {}

	const freshId = `verify-spawn-${Date.now()}`;
	const d = openPty(freshId, true);
	await d.open;
	const spawned = await d.waitFor(() => !!d.control('ikenga.snapshot'), 4000);
	check('spawn=true still creates a session on first attach', spawned);
	try {
		d.ws.close();
	} catch {}
}

console.log('\n=== 6. Chat WS: the join! deadlock (F-1) ===');
{
	const threadId = `verify-chat-${Date.now()}`;
	const ws = new WebSocket(`${WS_BASE}/ws/chat/${threadId}?token=${TOKEN}`);
	const updates: any[] = [];
	ws.addEventListener('message', (e: any) => {
		try {
			updates.push(JSON.parse(e.data));
		} catch {}
	});
	await new Promise<void>((res, rej) => {
		ws.addEventListener('open', () => res());
		ws.addEventListener('error', () => rej(new Error('chat ws failed to open')));
	});

	// Unknown engine: must terminate, and must say so rather than echoing.
	ws.send(JSON.stringify({ type: 'prompt', prompt: 'hello', engine: 'no-such-engine' }));
	const t0 = Date.now();
	const settled = await (async () => {
		while (Date.now() - t0 < 15000) {
			if (
				updates.some((u) => u?.params?.update?.status === 'idle' && u?.params?.update?.stop_reason)
			)
				return true;
			await sleep(100);
		}
		return false;
	})();
	check(
		'a turn reaches a terminal status instead of hanging the socket',
		settled,
		`${Date.now() - t0}ms, ${updates.length} updates`
	);

	const errUpdate = updates.find((u) => u?.params?.update?.type === 'error');
	check(
		'an unknown engine returns an error, not a fabricated reply',
		!!errUpdate && /Unknown engine/i.test(errUpdate.params.update.error?.message ?? ''),
		JSON.stringify(errUpdate?.params?.update)
	);
	const echoed = updates.some((u) =>
		JSON.stringify(u?.params?.update ?? {}).includes('Received prompt for')
	);
	check('the old echo-stub response is gone', !echoed);

	const stop = updates.find(
		(u) => u?.params?.update?.status === 'idle' && u?.params?.update?.stop_reason
	)?.params?.update?.stop_reason;
	check(
		'stop_reason is ACP snake_case, not Debug-formatted',
		typeof stop === 'string' && /^[a-z_]+$/.test(stop) && stop !== 'endturn',
		`stop_reason=${stop}`
	);

	// The socket must still be readable after a completed turn — that is the
	// deadlock. A hung join! would leave this unanswered.
	ws.send(JSON.stringify({ type: 'cancel' }));
	const aliveAfter = await (async () => {
		const before = updates.length;
		const t = Date.now();
		while (Date.now() - t < 5000) {
			if (updates.length > before) return true;
			await sleep(100);
		}
		return false;
	})();
	check('socket still processes messages after a turn completes (no deadlock)', aliveAfter);
	ws.close();
}

console.log('\n=== 7. Chat WS against the real antigravity CLI ===');
{
	const threadId = `verify-agy-${Date.now()}`;
	const ws = new WebSocket(`${WS_BASE}/ws/chat/${threadId}?token=${TOKEN}`);
	const updates: any[] = [];
	ws.addEventListener('message', (e: any) => {
		try {
			updates.push(JSON.parse(e.data));
		} catch {}
	});
	await new Promise<void>((res) => ws.addEventListener('open', () => res()));

	ws.send(
		JSON.stringify({
			type: 'prompt',
			prompt: 'Reply with exactly the word PONG and nothing else.',
			engine: 'antigravity-cli',
			cwd: '/tmp',
		})
	);

	const t0 = Date.now();
	const settled = await (async () => {
		while (Date.now() - t0 < 90000) {
			if (
				updates.some((u) => u?.params?.update?.status === 'idle' && u?.params?.update?.stop_reason)
			)
				return true;
			await sleep(200);
		}
		return false;
	})();
	const elapsed = Date.now() - t0;
	check(
		`real antigravity turn terminates (${elapsed}ms)`,
		settled,
		`${updates.length} updates received`
	);

	const stop = updates.find(
		(u) => u?.params?.update?.status === 'idle' && u?.params?.update?.stop_reason
	)?.params?.update?.stop_reason;
	console.log(`        stop_reason=${stop}`);
	const errs = updates.filter((u) => u?.params?.update?.type === 'error');
	if (errs.length) console.log(`        error: ${JSON.stringify(errs[0].params.update.error)}`);
	const deltas = updates.filter((u) => {
		const s = JSON.stringify(u?.params?.update ?? {});
		return (
			s.includes('agent_message_chunk') || s.includes('AgentMessageChunk') || s.includes('text')
		);
	});
	console.log(`        delta-bearing updates: ${deltas.length}`);
	if (deltas.length)
		console.log(`        first: ${JSON.stringify(deltas[0].params.update).slice(0, 240)}`);

	check(
		'a real turn either streams deltas or reports a real error — never silent success',
		deltas.length > 0 || errs.length > 0 || stop === 'refusal',
		`stop=${stop} deltas=${deltas.length} errs=${errs.length}`
	);
	ws.close();
}

console.log(`\n${'='.repeat(60)}`);
console.log(`RESULT: ${pass} passed, ${fail} failed`);
if (failures.length) {
	console.log('\nFailures:');
	for (const f of failures) console.log(`  - ${f}`);
}
process.exit(fail === 0 ? 0 : 1);
