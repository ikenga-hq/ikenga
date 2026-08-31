// Cron registry smoke test.
//
// 1. Install /tmp/test-pkg-com.example.cron (declares `* * * * * *` event:cron-tick).
// 2. Subscribe to `pkg://cron-tick`.
// 3. Wait ~2s — assert listener fires ≥1×.
// 4. Uninstall — record current count, wait ~2s more, assert no further fires.
import { createFileRoute } from '@tanstack/react-router';
import { listen, type UnlistenFn } from '@/lib/transport';
import { useEffect, useState } from 'react';

import { pkgInstallFromPath, pkgKernelStatus, pkgUninstall } from '@/lib/tauri-cmd';

export const Route = createFileRoute('/cron-smoke')({
	component: CronSmoke,
});

const PKG_PATH = '/tmp/test-pkg-com.example.cron';
const PKG_ID = 'com.example.cron';
const EVENT_NAME = 'pkg://cron-tick';

type Row = { label: string; outcome: string };

function CronSmoke() {
	const [rows, setRows] = useState<Row[]>([]);
	const [verdict, setVerdict] = useState('RUNNING');

	useEffect(() => {
		let cancelled = false;
		let unlisten: UnlistenFn | null = null;
		const log = (label: string, outcome: string) => {
			if (cancelled) return;
			// eslint-disable-next-line no-console
			console.log(`[cron-smoke] ${label}: ${outcome}`);
			setRows((p) => [...p, { label, outcome }]);
		};

		(async () => {
			try {
				await pkgUninstall(PKG_ID);
			} catch {
				// not installed
			}

			let count = 0;
			try {
				unlisten = await listen(EVENT_NAME, () => {
					count += 1;
				});
				log('LISTEN', `subscribed to ${EVENT_NAME}`);
			} catch (e) {
				log('LISTEN', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			let installed = false;
			try {
				const r = await pkgInstallFromPath(PKG_PATH);
				installed = true;
				log('INSTALL', `OK id=${r.installed.id}`);
			} catch (e) {
				log('INSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			try {
				const s = await pkgKernelStatus();
				const reg = s.registries.cron as
					| {
							entries: Array<{ pkg_id: string; cron_id: string; expr: string; handler: string }>;
							count: number;
					  }
					| undefined;
				const mine = (reg?.entries ?? []).filter((e) => e.pkg_id === PKG_ID);
				log(
					'REGISTRY',
					`count=${reg?.count ?? 'n/a'} pkg_jobs=${mine.length} ${JSON.stringify(mine.map((m) => `${m.cron_id} ${m.expr}→${m.handler}`))}`
				);
			} catch (e) {
				log('REGISTRY', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			// Wait ~2.5s — every-second cron should fire at least once. Use > to
			// stay tolerant of strict-mode double-mount and any late delivery.
			log('WAIT', 'sleeping 2500ms');
			await new Promise((r) => setTimeout(r, 2500));

			const firedDuringInstall = count;
			const firedOk = firedDuringInstall > 0;
			log('FIRED', `count=${firedDuringInstall} ok=${firedOk}`);

			let stoppedOk = false;
			try {
				await pkgUninstall(PKG_ID);
				log('UNINSTALL', 'OK');
				const before = count;
				await new Promise((r) => setTimeout(r, 2500));
				const after = count;
				// After uninstall, no new ticks should land (allow at most 1 race-tick
				// emitted-but-not-yet-delivered before remove).
				stoppedOk = after - before <= 1;
				log(
					'POST-UNINSTALL',
					`before=${before} after=${after} delta=${after - before} stopped=${stoppedOk}`
				);
			} catch (e) {
				log('UNINSTALL', `FAIL ${(e as Error).message ?? String(e)}`);
			}

			const ok = installed && firedOk && stoppedOk;
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
			<h1 style={{ fontSize: 16, marginBottom: 12 }}>Smoke: Cron Registry</h1>
			<div data-testid="cron-smoke-verdict" style={{ marginBottom: 16, fontWeight: 'bold' }}>
				{verdict}
			</div>
			<ul style={{ listStyle: 'none', padding: 0 }}>
				{rows.map((r, i) => (
					<li key={i} data-testid={`cron-smoke-row-${i}`}>
						<strong>{r.label}</strong> — {r.outcome}
					</li>
				))}
			</ul>
		</div>
	);
}
