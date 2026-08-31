// Iyke routes registry smoke test. Proves a package's `iyke.routes` entries
// are reachable via the iyke HTTP server and dispatch to the right handler.
//
// 1. Install /tmp/test-pkg-com.example.iyke (declares /pkg/.../echo + /event).
// 2. Read iyke endpoint (port + bearer token).
// 3. POST to /pkg/com.example.iyke/echo — expect body echoed back.
// 4. Subscribe to `pkg://custom-thing`, POST to .../event — expect listener fires.
// 5. Uninstall and POST again — expect 404.
import { createFileRoute } from '@tanstack/react-router';
import { listen, type UnlistenFn } from '@/lib/transport';
import { useEffect, useState } from 'react';

import { iykeEndpoint, pkgInstallFromPath, pkgKernelStatus, pkgUninstall } from '@/lib/tauri-cmd';

export const Route = createFileRoute('/iyke-smoke')({
	component: IykeSmoke,
});

const PKG_PATH = '/tmp/test-pkg-com.example.iyke';
const PKG_ID = 'com.example.iyke';
const ECHO_PATH = `/pkg/${PKG_ID}/echo`;
const EVENT_PATH = `/pkg/${PKG_ID}/event`;
const EVENT_NAME = 'pkg://custom-thing';

type Row = { label: string; outcome: string };

function IykeSmoke() {
	const [rows, setRows] = useState<Row[]>([]);
	const [verdict, setVerdict] = useState('RUNNING');

	useEffect(() => {
		let cancelled = false;
		let unlisten: UnlistenFn | null = null;
		const log = (label: string, outcome: string) => {
			if (cancelled) return;
			// eslint-disable-next-line no-console
			console.log(`[iyke-smoke] ${label}: ${outcome}`);
			setRows((p) => [...p, { label, outcome }]);
		};

		(async () => {
			// Reset any stale install from a prior run.
			try {
				await pkgUninstall(PKG_ID);
			} catch {
				// not installed
			}

			// Endpoint info
			let url = '';
			let token = '';
			try {
				const ep = await iykeEndpoint();
				url = ep.url;
				token = ep.token;
				log('ENDPOINT', `OK ${url} token=${token.slice(0, 8)}…`);
			} catch (e) {
				log('ENDPOINT', `FAIL ${(e as Error).message ?? String(e)}`);
				setVerdict('VERDICT FAIL');
				return;
			}

			const post = async (path: string, body: unknown) => {
				const res = await fetch(`${url}${path}`, {
					method: 'POST',
					headers: {
						authorization: `Bearer ${token}`,
						'content-type': 'application/json',
					},
					body: JSON.stringify(body),
				});
				return { status: res.status, text: await res.text() };
			};

			// Sanity: pre-install POST should 404 (route not registered).
			try {
				const r = await post(ECHO_PATH, { ping: 1 });
				log('PRE-INSTALL ECHO', `status=${r.status} body=${r.text.slice(0, 80)}`);
			} catch (e) {
				log('PRE-INSTALL ECHO', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Subscribe to the event the package will emit.
			let eventCount = 0;
			let lastEvent: unknown = null;
			try {
				unlisten = await listen(EVENT_NAME, (e) => {
					eventCount += 1;
					lastEvent = e.payload;
				});
				log('LISTEN', `subscribed to ${EVENT_NAME}`);
			} catch (e) {
				log('LISTEN', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Install
			let installed = false;
			try {
				const r = await pkgInstallFromPath(PKG_PATH);
				installed = true;
				log('INSTALL', `OK id=${r.installed.id}`);
			} catch (e) {
				log('INSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Status: registry should report 2 routes for this pkg.
			try {
				const s = await pkgKernelStatus();
				const iyke = s.registries.iyke_routes as
					| {
							entries: Array<{ pkg_id: string; method: string; path: string; handler: string }>;
							count: number;
					  }
					| undefined;
				const mine = (iyke?.entries ?? []).filter((e) => e.pkg_id === PKG_ID);
				log(
					'REGISTRY',
					`count=${iyke?.count ?? 'n/a'} pkg_routes=${mine.length} ${JSON.stringify(mine.map((m) => `${m.method} ${m.path}→${m.handler}`))}`
				);
			} catch (e) {
				log('REGISTRY', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Echo handler
			let echoOk = false;
			try {
				const payload = { hello: 'world', n: 42 };
				const r = await post(ECHO_PATH, payload);
				echoOk = r.status === 200 && r.text === JSON.stringify(payload);
				log('ECHO', `status=${r.status} match=${echoOk} body=${r.text.slice(0, 80)}`);
			} catch (e) {
				log('ECHO', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Event emit handler
			let emitOk = false;
			try {
				const before = eventCount;
				const r = await post(EVENT_PATH, { ping: 1 });
				// Give Tauri a tick to deliver the event.
				await new Promise((res) => setTimeout(res, 150));
				// React strict-mode mounts effects twice in dev, so the listener can
				// fire 1× or 2× per emit. Just assert it incremented.
				emitOk = r.status === 202 && eventCount > before;
				log(
					'EVENT',
					`status=${r.status} event_count=${eventCount} (was ${before}) last=${JSON.stringify(lastEvent)}`
				);
			} catch (e) {
				log('EVENT', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Uninstall, then a final POST should 404 again (proves unregister wired).
			let postUninstall404 = false;
			try {
				await pkgUninstall(PKG_ID);
				log('UNINSTALL', 'OK');
				const r = await post(ECHO_PATH, { ping: 1 });
				postUninstall404 = r.status === 404;
				log('POST-UNINSTALL ECHO', `status=${r.status} (expect 404) body=${r.text.slice(0, 80)}`);
			} catch (e) {
				log('UNINSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			const ok = installed && echoOk && emitOk && postUninstall404;
			const v = ok ? 'VERDICT PASS' : 'VERDICT FAIL';
			log('DONE', v);
			if (!cancelled) setVerdict(v);
		})();

		return () => {
			cancelled = true;
			if (unlisten) unlisten();
		};
	}, []);

	return (
		<div style={{ padding: 24, fontFamily: 'monospace', fontSize: 13 }}>
			<h1 style={{ fontSize: 16, marginBottom: 12 }}>Smoke: Iyke Routes Registry</h1>
			<div data-testid="iyke-smoke-verdict" style={{ marginBottom: 16, fontWeight: 'bold' }}>
				{verdict}
			</div>
			<ul style={{ listStyle: 'none', padding: 0 }}>
				{rows.map((r, i) => (
					<li key={i} data-testid={`iyke-smoke-row-${i}`}>
						<strong>{r.label}</strong> — {r.outcome}
					</li>
				))}
			</ul>
		</div>
	);
}
